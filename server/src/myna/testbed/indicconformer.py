"""AI4Bharat IndicConformer behind ``SttService``, on ONNX Runtime with no torch.

One language of AI4Bharat's multilingual hybrid RNN-T/CTC Conformer, cut out
of the checkpoint by ``dev/indicconformer/export_indicconformer_onnx.py``.
The model is offline (full attention), so it streams the way Parakeet does:
SilenceCut decodes each region once at a pause and commits it whole, and
between cuts the uncommitted window is re-decoded for unstable display text.
Batch mode decodes the same bounded regions and shows the text at the end.

The model directory:

- ``encoder-model[.int8].onnx``: mel (B, 80, T) to frames (B, 512, T/4).
- ``decoder_joint-model[.int8].onnx``: RNN-T prediction net and joint.
- ``ctc-model.onnx``: the CTC head, frames to log-probabilities.
- ``mel-model.onnx``: the checkpoint's mel filterbank and log.
- ``vocab.txt``, ``config.json``: tokens (blank last) and the preprocessor.

The int8 graphs are used when present. The transcript is unpunctuated and,
the scripts being caseless, uncased: the model was trained that way.

Needs onnxruntime alone; the ``indicconformer`` extra carries the same pin as
``parakeet``.
"""

from __future__ import annotations

import asyncio
import gc
import json
import os
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

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
from myna.testbed.streaming.strategies import (
    SC_ARM_S,
    SC_FORCE_CUT_S,
    SC_SILENCE_CUT_S,
    Hypothesis,
    Word,
)

RATE = 16_000
FORMAT = AudioFormat(sample_rate_hz=RATE, channels=1, sample_width_bytes=2)

Decoder = Literal["rnnt", "ctc"]
Floats = NDArray[np.float32]
DecodeFn = Callable[[Floats, float], Hypothesis]

# Parakeet's streaming shape: the same loop, the same cuts. A forced cut keeps
# a second of overlap for the loop to deduplicate; partials re-decode the
# uncommitted window every 2 s.
STREAM_OVERLAP_S = 1.0
PARTIAL_CADENCE_S = 2.0
_PROGRESS_INTERVAL_S = 1.0
_LOAD_HEARTBEAT_S = 2.0

# The encoder's threads: the pool ORT would pick is the machine's width,
# which buys nothing on a model this size and pins.
ENCODER_THREADS = 4


@dataclass(frozen=True)
class MelSpec:
    """NeMo's ``AudioToMelSpectrogramPreprocessor`` settings, dither off.
    ``filterbank`` maps a power spectrum (bins, T) to log-mel (features, T):
    an ORT session, since a numpy matmul wakes OpenBLAS, whose idle threads
    then spin on every core (4x this adapter's CPU, measured 2026-11-17)."""

    filterbank: Callable[[Floats], Floats]
    win_length: int
    n_fft: int
    hop: int
    preemph: float

    @classmethod
    def load(cls, config: dict[str, Any], filterbank: Callable[[Floats], Floats]) -> MelSpec:
        pre = config["preprocessor"]
        if pre.get("normalize") != "per_feature":
            raise ValueError(f"unsupported mel normalisation {pre.get('normalize')!r}")
        return cls(
            filterbank=filterbank,
            win_length=int(pre["win_length"]),
            n_fft=int(pre["n_fft"]),
            hop=int(pre["hop"]),
            preemph=float(pre["preemph"]),
        )


def log_mel(samples: Floats, spec: MelSpec) -> tuple[Floats, int]:
    """NeMo's normalised log-mel of ``samples``: (features, frames) and the
    count of valid frames. A centred STFT gives one frame more than NeMo
    counts valid; it is zeroed, as NeMo masks it, and left out of the
    statistics."""
    x = samples.astype(np.float32)
    if len(x):
        x = np.concatenate([x[:1], x[1:] - spec.preemph * x[:-1]])
    half = spec.n_fft // 2
    padded = np.pad(x, (half, half))
    frames = np.lib.stride_tricks.sliding_window_view(padded, spec.n_fft)[:: spec.hop]
    # A symmetric Hann, centred in the n_fft frame as torch.stft centres it.
    lead = (spec.n_fft - spec.win_length) // 2
    window = np.zeros(spec.n_fft, np.float32)
    window[lead : lead + spec.win_length] = np.hanning(spec.win_length)
    spectrum = np.fft.rfft(frames * window, axis=-1)
    power = (spectrum.real**2 + spectrum.imag**2).astype(np.float32)
    mel = spec.filterbank(np.ascontiguousarray(power.T))
    valid = len(x) // spec.hop
    if valid > 0:
        body = mel[:, :valid]
        mean = body.mean(axis=1, keepdims=True)
        std = body.std(axis=1, ddof=1, keepdims=True) if valid > 1 else np.zeros_like(mean)
        mel = (mel - mean) / (std + 1e-5)
    mel[:, valid:] = 0.0
    return mel.astype(np.float32), valid


def load_vocab(path: Path) -> list[str]:
    """Pieces by id from ``vocab.txt`` ("piece id" per line), ▁ as a space."""
    pieces: dict[int, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        piece, _, index = line.rpartition(" ")
        pieces[int(index)] = piece.replace("▁", " ")
    return [pieces[i] for i in range(len(pieces))]


def ctc_greedy(logprobs: Floats, blank: int) -> list[tuple[int, int]]:
    """(token, frame) per emitted token: the frame-wise argmax with repeats
    collapsed and blanks dropped."""
    best = logprobs.argmax(axis=-1)
    out: list[tuple[int, int]] = []
    prev = blank
    for frame, token in enumerate(best.tolist()):
        if token != blank and token != prev:
            out.append((token, frame))
        prev = token
    return out


def to_words(
    pieces: list[str], tokens: list[tuple[int, int]], frame_s: float, offset: float
) -> list[Word]:
    """Words from (token, frame) pairs. A piece that starts with a space
    begins a word and keeps the space, so committed regions concatenate
    verbatim; a word ends where the next begins."""
    words: list[Word] = []
    for token, frame in tokens:
        piece = pieces[token]
        start = offset + frame * frame_s
        if piece.startswith(" ") or not words:
            words.append(Word(text=piece, start=start, end=start + frame_s))
        else:
            last = words[-1]
            words[-1] = Word(text=last.text + piece, start=last.start, end=start + frame_s)
    return [w for w in words if w.text.strip()]


class _Model:
    """The ONNX sessions, shared by every session on this server."""

    def __init__(self, model_dir: Path, decoder: Decoder, encoder_threads: int) -> None:
        import onnxruntime as ort

        def session(path: Path, threads: int) -> Any:
            opts = ort.SessionOptions()
            opts.intra_op_num_threads = threads
            opts.inter_op_num_threads = 1
            return ort.InferenceSession(str(path), opts, providers=["CPUExecutionProvider"])

        def graph(stem: str, threads: int) -> Any:
            int8 = model_dir / f"{stem}.int8.onnx"
            return session(int8 if int8.is_file() else model_dir / f"{stem}.onnx", threads)

        config = json.loads((model_dir / "config.json").read_text(encoding="utf-8"))
        self.filterbank = session(model_dir / "mel-model.onnx", 1)
        self.mel = MelSpec.load(config, self._log_mel)
        self.pieces = load_vocab(model_dir / "vocab.txt")
        self.blank = len(self.pieces) - 1
        self.max_symbols = int(config.get("max_symbols", 10))
        self.frame_s = int(config["subsampling_factor"]) * self.mel.hop / RATE
        self.precision = "int8" if (model_dir / "encoder-model.int8.onnx").is_file() else "fp32"
        self.decoder = decoder
        self.encoder = graph("encoder-model", encoder_threads)
        if decoder == "ctc":
            self.ctc = session(model_dir / "ctc-model.onnx", 1)
        else:
            self.joint = graph("decoder_joint-model", 1)
            # (layers, batch, width): one LSTM layer on large, two on 600m.
            layers, _batch, width = self.joint.get_inputs()[3].shape
            self.state_shape = (int(layers), 1, int(width))

    def _log_mel(self, power: Floats) -> Floats:
        (logmel,) = self.filterbank.run(None, {"power": power[None]})
        return np.asarray(logmel[0], np.float32)

    def encode(self, samples: Floats) -> Floats:
        """Encoder frames (512, T') for ``samples``."""
        mel, valid = log_mel(samples, self.mel)
        encoded, lengths = self.encoder.run(
            None, {"audio_signal": mel[None], "length": np.array([valid], np.int64)}
        )
        return np.asarray(encoded[0, :, : int(lengths[0])], np.float32)

    def _rnnt(self, frames: Floats) -> list[tuple[int, int]]:
        """Greedy RNN-T: at each frame emit until blank or ``max_symbols``;
        only an emitted token advances the prediction net. The blank is the
        start symbol: its embedding row is the zero pad."""
        zeros = np.zeros(self.state_shape, np.float32)
        state = (zeros, zeros)
        prev = self.blank
        out: list[tuple[int, int]] = []
        for t in range(frames.shape[1]):
            frame = frames[None, :, t : t + 1]
            for _ in range(self.max_symbols):
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
                token = int(np.argmax(logits))
                if token == self.blank:
                    break
                out.append((token, t))
                prev, state = token, (s1, s2)
        return out

    def transcribe_words(self, samples: Floats, offset: float = 0.0) -> list[Word]:
        frames = self.encode(samples)
        if self.decoder == "ctc":
            (logprobs,) = self.ctc.run(None, {"encoder_outputs": frames[None]})
            tokens = ctc_greedy(logprobs[0], self.blank)
        else:
            tokens = self._rnnt(frames)
        return to_words(self.pieces, tokens, self.frame_s, offset)


def _default_model_dir() -> str:
    env = os.environ.get("MODEL_DIR")
    if env:
        return env
    return str(Path.home() / ".cache" / "myna" / "models" / "indicconformer-large-hi")


def _language(model_dir: Path) -> str:
    """The language a model dir serves, from its name's last part."""
    return model_dir.name.rsplit("-", 1)[-1]


class IndicConformerAdapter:
    """IndicConformer ONNX behind ``SttService``; chunked-commit streaming."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        streaming: bool = False,
        decoder: Decoder = "ctc",
        encoder_threads: int = ENCODER_THREADS,
        stream_arm_s: float = SC_ARM_S,
        stream_silence_cut_s: float = SC_SILENCE_CUT_S,
        stream_force_cut_s: float = SC_FORCE_CUT_S,
        stream_partial_cadence_s: float = PARTIAL_CADENCE_S,
    ) -> None:
        if decoder not in ("rnnt", "ctc"):
            raise ValueError(f"decoder must be rnnt or ctc, got {decoder!r}")
        self._model_dir = Path(model_dir or _default_model_dir())
        self._streaming = streaming
        self._decoder: Decoder = decoder
        self._encoder_threads = encoder_threads
        self._stream_arm_s = stream_arm_s
        self._stream_silence_cut_s = stream_silence_cut_s
        self._stream_force_cut_s = stream_force_cut_s
        self._stream_partial_cadence_s = stream_partial_cadence_s
        self._model: _Model | None = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        return self._streaming

    @property
    def candidate(self) -> Candidate:
        return Candidate(
            model=self._model_dir.name,
            engine=f"onnxruntime-cpu-{self._decoder}",
            streaming_strategy="chunked-commit" if self._streaming else "commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=(self.candidate.model,),
            languages=(_language(self._model_dir),),
            input_formats=(FORMAT,),
            punctuation=False,
            translation=False,
            streaming=self._streaming,
            runtime={
                **runtime_versions(("onnxruntime", "onnxruntime")),
                "execution_provider": "CPUExecutionProvider",
            },
        )

    async def _load_model(self) -> _Model:
        async with self._model_lock:
            if self._model is None:
                self._model = await asyncio.to_thread(
                    _Model, self._model_dir, self._decoder, self._encoder_threads
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

            def decode(samples: Floats, offset: float) -> Hypothesis:
                return Hypothesis(words=model.transcribe_words(samples, offset))

            if self._streaming:
                await self._run_streaming(audio, emit, decode)
                return
            text = await self._run_batch(audio, emit, decode)
            # An empty final would count as a committed segment.
            if text:
                await emit(TranscriptionFinal(text=text, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    async def _run_batch(
        self,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
        decode: DecodeFn,
    ) -> str:
        from myna.testbed.streaming.batch import run_deferred_batch

        parts: list[str] = []

        async def on_commit(text: str, _words: list[Word]) -> None:
            parts.append(text)

        await run_deferred_batch(audio, emit, decode, on_commit)
        return "".join(parts).strip()

    async def _run_streaming(
        self,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
        decode: DecodeFn,
    ) -> None:
        from myna.testbed.streaming.loop import run_streaming_loop
        from myna.testbed.streaming.strategies import SilenceCut

        transcript = await run_streaming_loop(
            audio,
            emit,
            decode,
            SilenceCut(
                arm_seconds=self._stream_arm_s,
                silence_cut_seconds=self._stream_silence_cut_s,
                force_cut_seconds=self._stream_force_cut_s,
            ),
            cadence_seconds=_PROGRESS_INTERVAL_S,
            # The force cut is the memory bound (I6) in chunked mode.
            window_cap_seconds=self._stream_force_cut_s + 5.0,
            overlap_seconds=STREAM_OVERLAP_S,
            partial_cadence_seconds=self._stream_partial_cadence_s or None,
        )
        await emit(TranscriptionDone(text=transcript))
