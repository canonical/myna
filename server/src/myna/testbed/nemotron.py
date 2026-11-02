"""NVIDIA Nemotron cache-aware streaming behind ``SttService``, on ONNX
Runtime with no torch.

A natively streaming transducer: each encoder step takes one chunk of new
audio (0.56 s at right context 6), carries its attention and convolution
caches into the next, and its tokens are never revised. So every token commits
the step it is decoded, a word lands about half a second after it is spoken,
and a step costs the same at minute one as at minute sixty.

The model directory, as ``dev/nemotron/export_nemotron_onnx.py`` and
``dev/nemotron/raw_mel.py`` leave it:

- ``encoder-model[.int8].onnx``: cache-aware encoder, fixed to one chunk shape
  with ``drop_extra_pre_encoded`` baked in, so every step, the first included,
  is stepped NeMo's ``pad_and_drop_preencoded`` way.
- ``decoder_joint-model[.int8].onnx``: RNN-T prediction net and joint.
- ``mel.onnx``: raw log-mel (Nemotron's preprocessor is ``normalize: NA``).
- ``prompt-model.onnx``: the 3.5 models' language conditioning, absent on
  the English one.
- ``vocab.txt``, ``config.json``: tokens (blank last) and the streaming shape.

The int8 graphs are used when present. Checked against NeMo on 2026-11-02 with
``dev/nemotron/parity.py``: fp32 token-identical over whole clips; the int8
export (the encoder's MatMuls only) scores within noise of fp32 at 0.2 core.

Needs onnxruntime alone; the ``parakeet`` extra carries the same pin.
"""

from __future__ import annotations

import asyncio
import gc
import json
import os
import re
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np
from numpy.typing import NDArray

from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    AudioFormat,
    Capabilities,
    Disposition,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate, runtime_versions

RATE = 16_000
FORMAT = AudioFormat(sample_rate_hz=RATE, channels=1, sample_width_bytes=2)

# Audio appended at the end so the last words get the right context they wait
# for: 6 frames of right context at 8x subsampling is 0.48 s, and a chunk
# boundary can add up to 0.56 s more.
FLUSH_S = 2.0

# NeMo's greedy RNN-T default: tokens one encoder frame may emit.
MAX_SYMBOLS = 10

# Two threads for the encoder: under torch, four spun for 4.6x the CPU for
# the same latency (captions feasibility run, 2026-11-02).
ENCODER_THREADS = 2

_LOAD_HEARTBEAT_S = 2.0

# The 3.5 models write the language after terminal punctuation ("<fr-FR>")
# even when it is set, as a word of its own: its leading space goes with it,
# or "friend? <en-US> He" reads "friend?  He".
_LANGUAGE_TAG = re.compile(r" ?<[a-z]{2,3}-[A-Z]{2}>")

# A session names its language by code; the 3.5 prompts are locales.
_LOCALES = {
    "en": "en-US",
    "fr": "fr-FR",
    "es": "es-ES",
    "de": "de-DE",
    "it": "it-IT",
    "pt": "pt-BR",
    "nl": "nl-NL",
    "tr": "tr-TR",
    "ru": "ru-RU",
    "ar": "ar-AR",
    "hi": "hi-IN",
    "ja": "ja-JP",
    "ko": "ko-KR",
    "vi": "vi-VN",
    "uk": "uk-UA",
}

Floats = NDArray[np.float32]


class MelStream:
    """Log-mel frames over a growing signal, each identical to the frame a
    pass over the whole signal gives it.

    ``mel(samples)`` returns (features, frames) for a centred STFT, frame k
    centred on sample k * hop. A frame is computed once its window lies wholly
    in the audio, from a slice that starts whole hops before it, so neither
    the slice's edge padding nor a filter's warm-up at its first sample can
    reach it.
    """

    def __init__(self, mel: Callable[[Floats], Floats], hop: int, n_fft: int) -> None:
        self._mel = mel
        self._hop = hop
        self._half = n_fft // 2
        self._context = (
            -(-self._half // hop) + 1
        )  # hops the window reaches, and one for a pre-emphasis tap
        self._audio = np.zeros(0, np.float32)
        self._audio_origin = 0  # samples trimmed from the front
        self._frames: Floats | None = None
        self.origin = 0  # frame index of the first frame kept
        self.count = 0  # frames computed

    def push(self, samples: Floats) -> None:
        self._audio = np.concatenate([self._audio, samples])
        total = self._audio_origin + len(self._audio)
        if total < self._half:
            return
        ready = (total - self._half) // self._hop + 1
        if ready <= self.count:
            return
        lo = max(0, (self.count - self._context) * self._hop)
        feats = self._mel(self._audio[lo - self._audio_origin :])
        first = (self.count * self._hop - lo) // self._hop
        new = feats[:, first : first + ready - self.count]
        self._frames = new if self._frames is None else np.concatenate([self._frames, new], axis=1)
        self.count = ready
        drop = max(0, (self.count - self._context) * self._hop - self._audio_origin)
        self._audio = self._audio[drop:]
        self._audio_origin += drop

    def frames(self, start: int, stop: int, features: int) -> Floats:
        """Frames [start, stop), zeros before the signal (NeMo pads the first
        chunk's pre-encode cache with zeros)."""
        lead = max(0, -start)
        if self._frames is None:
            got = np.zeros((features, 0), np.float32)
        else:
            got = self._frames[:, max(start, 0) - self.origin : stop - self.origin]
        if lead:
            got = np.concatenate([np.zeros((got.shape[0], lead), np.float32), got], axis=1)
        return got

    def release(self, before: int) -> None:
        """Forget frames before ``before``."""
        if self._frames is not None and before > self.origin:
            self._frames = self._frames[:, before - self.origin :]
            self.origin = before


@dataclass(frozen=True)
class Shape:
    """One encoder step's input: ``chunk`` new mel frames after
    ``pre_cache`` already seen."""

    chunk: int
    pre_cache: int
    features: int
    hop: int
    n_fft: int

    @classmethod
    def from_config(cls, config: dict[str, Any]) -> Shape:
        streaming, pre = config["streaming"], config["preprocessor"]

        def steady(value: int | list[int]) -> int:
            return value[1] if isinstance(value, list) else value

        return cls(
            chunk=steady(streaming["chunk_size"]),
            pre_cache=steady(streaming["pre_encode_cache_size"]),
            features=int(pre["features"]),
            hop=round(pre["window_stride"] * RATE),
            n_fft=int(pre["n_fft"]),
        )


JointFn = Callable[[Floats, int, tuple[Floats, Floats]], tuple[Floats, tuple[Floats, Floats]]]


@dataclass
class Decoder:
    """Greedy RNN-T state carried across steps."""

    blank: int
    state: tuple[Floats, Floats]
    prev: int = -1

    def __post_init__(self) -> None:
        if self.prev < 0:
            self.prev = self.blank

    def decode(self, frames: Floats, joint: JointFn) -> list[int]:
        """Tokens for encoder ``frames`` (features, time): at each frame,
        emit until blank or ``MAX_SYMBOLS``; only an emitted token advances
        the prediction net."""
        tokens: list[int] = []
        for t in range(frames.shape[1]):
            frame = frames[None, :, t : t + 1]
            for _ in range(MAX_SYMBOLS):
                logits, state = joint(frame, self.prev, self.state)
                token = int(np.argmax(logits))
                if token == self.blank:
                    break
                tokens.append(token)
                self.prev, self.state = token, state
        return tokens


def load_vocab(path: Path) -> list[str]:
    """Pieces by id from ``vocab.txt`` ("piece id" per line)."""
    pieces: dict[int, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        piece, _, index = line.rpartition(" ")
        pieces[int(index)] = piece
    return [pieces[i] for i in range(len(pieces))]


def detokenize(pieces: list[str], tokens: list[int]) -> str:
    """Text for ``tokens``, word starts as spaces, language tags dropped."""
    text = "".join(pieces[t] for t in tokens).replace("▁", " ")
    return _LANGUAGE_TAG.sub("", text)


class _Model:
    """The ONNX sessions, shared by every stream on this server."""

    def __init__(self, model_dir: Path, encoder_threads: int) -> None:
        import onnxruntime as ort

        def opts(threads: int) -> Any:
            o = ort.SessionOptions()
            o.intra_op_num_threads = threads
            o.inter_op_num_threads = 1
            return o

        def graph(stem: str, threads: int) -> Any:
            int8 = model_dir / f"{stem}.int8.onnx"
            path = int8 if int8.is_file() else model_dir / f"{stem}.onnx"
            return ort.InferenceSession(
                str(path), opts(threads), providers=["CPUExecutionProvider"]
            )

        self.config = json.loads((model_dir / "config.json").read_text())
        self.shape = Shape.from_config(self.config)
        self.pieces = load_vocab(model_dir / "vocab.txt")
        self.precision = "int8" if (model_dir / "encoder-model.int8.onnx").is_file() else "fp32"
        self.encoder = graph("encoder-model", encoder_threads)
        self.joint = graph("decoder_joint-model", 1)
        self.mel = ort.InferenceSession(
            str(model_dir / "mel.onnx"), opts(1), providers=["CPUExecutionProvider"]
        )
        prompt = model_dir / "prompt-model.onnx"
        self.prompt = (
            ort.InferenceSession(str(prompt), opts(1), providers=["CPUExecutionProvider"])
            if prompt.is_file()
            else None
        )
        self.prompts: dict[str, int] = self.config.get("prompts") or {}
        self.cache_shapes: dict[str, list[int]] = self.config["cache_shapes"]
        self.state_width = int(self.joint.get_inputs()[3].shape[-1])
        self.blank = len(self.pieces) - 1

    def log_mel(self, samples: Floats) -> Floats:
        (features,) = self.mel.run(
            ["features"],
            {"waveforms": samples[None], "waveforms_lens": np.array([len(samples)], np.int64)},
        )
        return np.asarray(features[0], np.float32)

    def joint_step(
        self, frame: Floats, prev: int, state: tuple[Floats, Floats]
    ) -> tuple[Floats, tuple[Floats, Floats]]:
        logits, _lengths, s1, s2 = self.joint.run(
            None,
            {
                "encoder_outputs": frame,
                "targets": np.array([[prev]], np.int32),
                "target_length": np.array([1], np.int32),
                "input_states_1": state[0],
                "input_states_2": state[1],
            },
        )
        return logits.reshape(-1), (s1, s2)

    def prompt_vector(self, language: str | None) -> Floats | None:
        """The one-hot language prompt for a 3.5 model; None for one without."""
        if self.prompt is None:
            return None
        width = int(self.prompt.get_inputs()[1].shape[-1])
        locale = (
            language if language and "-" in language else _LOCALES.get((language or "").lower())
        )
        index = self.prompts.get(locale or "", self.prompts.get("auto", 0))
        vector = np.zeros((1, width), np.float32)
        vector[0, index] = 1.0
        return vector


@dataclass
class Stream:
    """One session: its mel, encoder caches and decoder state."""

    model: _Model
    prompt: Floats | None = None
    mel: MelStream = field(init=False)
    decoder: Decoder = field(init=False)
    caches: dict[str, NDArray[Any]] = field(init=False)
    next_frame: int = 0

    def __post_init__(self) -> None:
        shape = self.model.shape
        self.mel = MelStream(self.model.log_mel, shape.hop, shape.n_fft)
        width = self.model.state_width
        self.decoder = Decoder(
            blank=self.model.blank,
            state=(np.zeros((2, 1, width), np.float32), np.zeros((2, 1, width), np.float32)),
        )
        self.caches = {
            name: np.zeros(dims, np.int64 if name.endswith("_len") else np.float32)
            for name, dims in self.model.cache_shapes.items()
        }

    def feed(self, samples: Floats) -> str:
        """Append audio; step every chunk it completes; return the new text."""
        self.mel.push(samples)
        shape = self.model.shape
        tokens: list[int] = []
        while self.next_frame + shape.chunk <= self.mel.count:
            tokens += self._step()
            self.next_frame += shape.chunk
            self.mel.release(self.next_frame - shape.pre_cache)
        return detokenize(self.model.pieces, tokens)

    def finish(self) -> str:
        return self.feed(np.zeros(int(FLUSH_S * RATE), np.float32))

    def _step(self) -> list[int]:
        shape = self.model.shape
        x = self.mel.frames(
            self.next_frame - shape.pre_cache, self.next_frame + shape.chunk, shape.features
        )[None]
        outputs = self.model.encoder.run(
            None,
            {
                "audio_signal": x,
                "length": np.array([x.shape[-1]], np.int64),
                **self.caches,
            },
        )
        encoded, lengths = outputs[0], outputs[1]
        # The next caches come out in the order the caches go in.
        for name, value in zip(self.model.cache_shapes, outputs[2:], strict=True):
            self.caches[name] = value
        frames = encoded[:, :, : int(lengths[0])]
        if self.prompt is not None:
            assert self.model.prompt is not None
            (frames,) = self.model.prompt.run(
                ["outputs"], {"encoder_outputs": frames, "prompt": self.prompt}
            )
        return self.decoder.decode(frames[0], self.model.joint_step)


def _default_model_dir() -> str:
    env = os.environ.get("MODEL_DIR")
    if env:
        return env
    return str(Path.home() / ".cache" / "myna" / "models" / "nemotron-en-rc6")


class NemotronAdapter:
    """Nemotron cache-aware streaming ONNX behind ``SttService``."""

    def __init__(
        self, model_dir: str | None = None, *, encoder_threads: int = ENCODER_THREADS
    ) -> None:
        self._model_dir = Path(model_dir or _default_model_dir())
        self._encoder_threads = encoder_threads
        self._model: _Model | None = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        return True

    @property
    def candidate(self) -> Candidate:
        return Candidate(
            model=self._model_dir.name,
            engine="onnxruntime-cpu",
            streaming_strategy="native-transducer",
        )

    def capabilities(self) -> Capabilities:
        prompts = self._model.prompts if self._model else {}
        languages = tuple(sorted({code for code, loc in _LOCALES.items() if loc in prompts})) or (
            "en",
        )
        return Capabilities(
            models=(self.candidate.model,),
            languages=languages,
            input_formats=(FORMAT,),
            punctuation=True,
            translation=False,
            streaming=True,
            runtime={
                **runtime_versions(("onnxruntime", "onnxruntime")),
                "execution_provider": "CPUExecutionProvider",
            },
        )

    async def _load_model(self) -> _Model:
        async with self._model_lock:
            if self._model is None:
                self._model = await asyncio.to_thread(
                    _Model, self._model_dir, self._encoder_threads
                )
            return self._model

    async def unload(self) -> None:
        """Release the ONNX sessions (idle unload). Idempotent."""
        async with self._model_lock:
            self._model = None
        gc.collect()

    async def _load_with_heartbeat(self, emit: EventSink) -> _Model:
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_S)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        model = await load
        await emit(TranscriptionProgress(phase=PHASE_READY))
        return model

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=(
                        f"need {RATE} Hz mono S16LE, got {fmt.sample_rate_hz} Hz "
                        f"{fmt.channels}ch {8 * fmt.sample_width_bytes}-bit"
                    ),
                )
            )
            return
        try:
            model = await self._load_with_heartbeat(emit)
            stream = Stream(model, prompt=model.prompt_vector(config.language))
            committed: list[str] = []

            async def commit(text: str) -> None:
                if not committed:
                    text = text.lstrip()
                if text:
                    await emit(
                        TranscriptionFinal(
                            text=text,
                            disposition=Disposition.COMMITTED,
                            segment_index=len(committed),
                        )
                    )
                    committed.append(text)

            async for chunk in audio:
                samples = np.frombuffer(chunk.data, "<i2").astype(np.float32) / 32768.0
                await commit(await asyncio.to_thread(stream.feed, samples))
            await commit(await asyncio.to_thread(stream.finish))
            await emit(TranscriptionDone(text="".join(committed)))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )
