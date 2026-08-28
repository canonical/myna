"""Parakeet TDT 0.6B v3 adapter — int8 ONNX via onnxruntime, no torch (008 US3).

The CPU-tier transducer arm of the streaming investigation: a fraction of the
full NeMo snap's size (SC-005), decode cheap enough that chunked commit is
essentially free. The greedy TDT decode loop is a numpy/onnxruntime port of
murmure's ``engine.rs`` (itself a port of NVIDIA's reference decoder):
preprocess (nemo128 mel) → encode → per-frame ``decode_step`` with the
duration-head skip. Biasing/boost is deliberately not ported (out of scope —
spec 008).

Emission is **chunked commit** (murmure ``audio/chunking.rs`` semantics, ported
as ``streaming.strategies.SilenceCut``): speech accumulates until a pause cuts
it past the arm point (15 s arm / 500 ms silence cut / 60 s force cut with 1 s
overlap), each finalized chunk is decoded *once* and committed wholesale.
Chunk-final decode means no re-decode strategy buys anything for *committed*
text, and decode-once keeps streaming WER at batch parity (fixed-head's 008
control result). Committed text is therefore only as prompt as the arm: at the
shipped 15 s, measured live 2026-08-28, the first word reaches the screen 17.7 s
into a 28 s utterance. So the loop also re-decodes the uncommitted window on
``stream_partial_cadence_s`` and emits it as **unstable** display text (0 to
show nothing between cuts, the pre-2026-08-28 behaviour). Being
display-only it cannot move committed WER — measured identical — and it takes
time-to-first-word from 17.7 s to 0.6 s. All emission invariants (I1–I7) are
enforced by the shared ``streaming.loop``.

Requires the ``parakeet`` extra: ``uv sync --extra parakeet``. Weights are
murmure's ``parakeet-tdt-0.6b-v3-int8`` bundle, staged by
``dev/fetch_parakeet_onnx.py`` (pinned + sha256-verified) into the XDG cache;
``--model`` points at a local directory instead (snap model component).
The collapse this adapter was chosen to avoid is not avoided, and is not what
it was thought to be. Murmure's re-quantized int8 encoder was picked over
istupakov's because istupakov's collapses (blank output mid-audio)
non-monotonically on some inputs, read at the time as quantization failing to
absorb utterance-global CMVN shifting with window length (2026-07-29
discriminator runs). Re-measured 2026-08-28 over 486 sliding windows: murmure's
collapses on **11.5%** of them and istupakov's on 9.1%, so murmure's was never
the safe one — it was probed on windows that happened to work. Quantization is
not the mechanism either; see ``_COLLAPSE_WORDS_PER_SECOND`` for the evidence
and for the retry that recovers most of it.
"""

from __future__ import annotations

import asyncio
import os
import re
from collections.abc import AsyncIterator
from pathlib import Path

import numpy as np

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
from myna.testbed.adapter import Candidate
from myna.testbed.streaming.strategies import (
    SC_ARM_S,
    SC_FORCE_CUT_S,
    SC_SILENCE_CUT_S,
    Hypothesis,
    Word,
)

PARAKEET_RATE = 16_000
PARAKEET_FORMAT = AudioFormat(sample_rate_hz=PARAKEET_RATE, channels=1, sample_width_bytes=2)

MODEL_FILES = (
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
)

# TDT frame duration: 10 ms feature window × 8 encoder subsampling.
FRAME_S = 0.08
MAX_TOKENS_PER_STEP = 10  # RNNT multi-token-per-frame guard (murmure parity)

# Parakeet TDT 0.6B v3 is multilingual across 25 European languages,
# auto-detected (no language input to the ONNX graph).
LANGUAGES = (
    "bg",
    "cs",
    "da",
    "de",
    "el",
    "en",
    "es",
    "et",
    "fi",
    "fr",
    "hr",
    "hu",
    "it",
    "lt",
    "lv",
    "nl",
    "pl",
    "pt",
    "ro",
    "ru",
    "sk",
    "sl",
    "sv",
    "uk",
)

_LOAD_HEARTBEAT_SECONDS = 2.0
_PROGRESS_INTERVAL_SECONDS = 1.0

# Parakeet TDT v3 returns near-empty output for particular window lengths: the
# joint emits blank at every frame with a +5 to +10 logit margin, on audio the
# same window half a second longer transcribes correctly. Measured 2026-08-28
# over 486 sliding windows (3-16 s) of the real corpus: 11.5% of decodes
# collapse. It is NOT the quantisation the module docstring blames for
# istupakov's failures — the 2.4 GB fp32 encoder collapses at 11.9%, on
# different windows — nor the TDT duration head, which capping does not fix;
# the encoder output for shared audio diverges (frame cosine 0.55) while the
# mel features agree (0.997). sherpa-onnx's export of the same weights
# collapses at 5.3%, so roughly half of ours is the port's to lose.
#
# Until that is understood, a plausibility check and one retry with the window
# nudged recovers 77% of collapses. Padding *every* decode is the wrong trade
# — always-on 0.2 s padding halves the collapse rate but costs 1.6 pp of WER
# on the healthy 88% (2.60% -> 4.20% batch, and 0.5 s padding costs 6.4 pp),
# which is why this fires only on a decode that already came back implausibly
# short for the audio it was given.
_COLLAPSE_WORDS_PER_SECOND = 0.5  # 5x below conversational speech (~2.5 w/s)
_COLLAPSE_RETRY_PAD_S = 0.2

# Unstable-partial dials. 0.5 s reads as continuous without the preedit region
# thrashing, and a tick is affordable at any window the arm allows: the decode
# costs ~22 ms + ~16 ms per second of audio, so even a full 15 s window is
# ~260 ms. Measured over seven streams: the median word is on screen 1.6 s
# after it is spoken, against 10.6 s with no partials.
#
# The tail cap is off by default (0 = decode the whole uncommitted window).
# Capping it is the way to buy compute back on a weak machine, at the cost of
# the display showing only the last N seconds — see [`_chunked_partial`], which
# carries the evidence for why the obvious middle ground does not work. Raising
# the cadence is the gentler dial: 1.0 s roughly halves the work.
PARTIAL_CADENCE_S = 0.5
PARTIAL_TAIL_S = 0.0

# Murmure's DECODE_SPACE_RE: strip the leading space and spaces before
# punctuation, keep word-boundary spaces. Tokens carry ▁→space already.
_DECODE_SPACE_RE = re.compile(r"\A\s|\s\B|(\s)\b")


def _detokenize(tokens: list[str]) -> str:
    text = "".join(tokens)
    return _DECODE_SPACE_RE.sub(lambda m: " " if m.group(1) else "", text)


def _load_vocab(model_dir: str) -> tuple[list[str], int]:
    """vocab.txt lines are ``token id``; ▁ becomes a literal space (murmure
    load_vocab parity). Returns (vocab by id, blank id)."""
    vocab: list[str] = []
    blank_idx = -1
    with open(os.path.join(model_dir, "vocab.txt"), encoding="utf-8") as fh:
        for line in fh:
            parts = line.rstrip("\n").split(" ")
            if len(parts) < 2:
                continue
            token, idx = parts[0], int(parts[1])
            while len(vocab) <= idx:
                vocab.append("")
            vocab[idx] = token.replace("▁", " ")
            if token == "<blk>":
                blank_idx = idx
    if blank_idx < 0:
        raise ValueError("vocab.txt has no <blk> token")
    return vocab, blank_idx


def _tokens_to_words(tokens: list[str], timestamps: list[float]) -> list[Word]:
    """Group subword tokens into words (a token starting with a space begins a
    new word), keeping each word's natural leading space so committed chunks
    concatenate verbatim (I2). Times are relative to the decoded region."""
    words: list[Word] = []
    for i, (token, start) in enumerate(zip(tokens, timestamps, strict=True)):
        end = timestamps[i + 1] if i + 1 < len(timestamps) else start + FRAME_S
        if token.startswith(" ") or not words:
            words.append(Word(text=token, start=start, end=end))
        else:
            words[-1] = Word(text=words[-1].text + token, start=words[-1].start, end=end)
    return words


def _encoder_threads() -> int:
    """Intra-op threads for the encoder.

    The encoder scales to about four and then goes backwards as SMT siblings
    contend, so this is half the visible CPUs, capped at four and floored at
    one. ``sched_getaffinity`` rather than ``cpu_count`` so a taskset or a
    cgroup cpuset is respected.
    """
    return max(1, min(4, len(os.sched_getaffinity(0)) // 2))


class _ParakeetOnnx:
    """The three ONNX sessions + greedy TDT decode (murmure engine.rs port)."""

    def __init__(self, model_dir: str, *, encoder_threads: int | None = None) -> None:
        import onnxruntime as ort

        providers = ["CPUExecutionProvider"]

        def opts(intra: int) -> ort.SessionOptions:
            o = ort.SessionOptions()
            o.log_severity_level = 3
            o.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
            o.intra_op_num_threads = intra
            o.inter_op_num_threads = 1
            return o

        # Sizing the three pools separately is worth ~2x on short windows and
        # 45 threads (measured 2026-08-28: 45 -> 3 added threads, and a 2 s
        # window from 112 ms to 55 ms). Left to itself ORT gives every session
        # a pool the width of the machine, so three sessions oversubscribe it
        # threefold and then fight. The preprocessor and the decoder_joint run
        # tensors far too small to divide: the joint's is one frame, and the
        # greedy loop calls it once per encoder frame — ~94 times for a 15 s
        # window — so its thread barriers were most of the cost of running it.
        # Threads are also not free at rest here: each one takes a glibc
        # malloc arena, which is where the RSS growth over a long session came
        # from (see myna.server.lifecycle on trimming them back).
        self._preprocessor = ort.InferenceSession(
            os.path.join(model_dir, "nemo128.onnx"), opts(1), providers=providers
        )
        self._encoder = ort.InferenceSession(
            os.path.join(model_dir, "encoder-model.int8.onnx"),
            opts(encoder_threads or _encoder_threads()),
            providers=providers,
        )
        self._decoder_joint = ort.InferenceSession(
            os.path.join(model_dir, "decoder_joint-model.int8.onnx"),
            opts(1),
            providers=providers,
        )
        self._vocab, self._blank_idx = _load_vocab(model_dir)
        # Logits are vocab + TDT duration bins; the duration slice is the tail.
        self._vocab_size = len(self._vocab)

    def _create_decoder_state(self) -> tuple[np.ndarray, np.ndarray]:
        return (
            np.zeros((2, 1, 640), dtype=np.float32),
            np.zeros((2, 1, 640), dtype=np.float32),
        )

    def _decode_step(
        self,
        prev_token: int,
        state: tuple[np.ndarray, np.ndarray],
        encoder_step: np.ndarray,  # [1024]
    ) -> tuple[np.ndarray, tuple[np.ndarray, np.ndarray]]:
        outputs = self._decoder_joint.run(
            ["outputs", "output_states_1", "output_states_2"],
            {
                "encoder_outputs": encoder_step.reshape(1, -1, 1).astype(np.float32),
                "targets": np.array([[prev_token]], dtype=np.int32),
                "target_length": np.array([1], dtype=np.int32),
                "input_states_1": state[0],
                "input_states_2": state[1],
            },
        )
        logits = outputs[0].reshape(-1)  # [1, 1, 1, vocab+durations] → flat
        return logits, (outputs[1], outputs[2])

    def _decode_sequence(
        self, encodings: np.ndarray, encodings_len: int
    ) -> tuple[list[str], list[float]]:
        """Greedy TDT decode (murmure decode_sequence_greedy): argmax vocab
        token; on non-blank update the decoder state; the duration head skips
        t forward; a blank (or MAX_TOKENS_PER_STEP at one frame) advances 1."""
        state = self._create_decoder_state()
        tokens: list[str] = []
        timestamps: list[float] = []
        t = 0
        emitted_at_frame = 0
        prev_token = self._blank_idx
        while t < encodings_len:
            logits, new_state = self._decode_step(prev_token, state, encodings[t])
            vocab_logits = logits[: self._vocab_size]
            dur_logits = logits[self._vocab_size :]
            token = int(np.argmax(vocab_logits))
            if token != self._blank_idx:
                state = new_state
                prev_token = token
                tokens.append(self._vocab[token])
                timestamps.append(t * FRAME_S)
                emitted_at_frame += 1
            duration = int(np.argmax(dur_logits)) if len(dur_logits) else 0
            if duration > 0:
                t += duration
                emitted_at_frame = 0
            elif token == self._blank_idx or emitted_at_frame == MAX_TOKENS_PER_STEP:
                t += 1
                emitted_at_frame = 0
        return tokens, timestamps

    def transcribe(self, samples: np.ndarray) -> tuple[list[str], list[float]]:
        """float32 mono 16 kHz → (tokens, token timestamps in region seconds)."""
        waveforms = samples.reshape(1, -1).astype(np.float32)
        waveforms_lens = np.array([samples.shape[0]], dtype=np.int64)
        features, features_lens = self._preprocessor.run(
            ["features", "features_lens"],
            {"waveforms": waveforms, "waveforms_lens": waveforms_lens},
        )
        encoder_out, encoder_lens = self._encoder.run(
            ["outputs", "encoded_lengths"],
            {"audio_signal": features, "length": features_lens},
        )
        # [1, 1024, T] → [1, T, 1024]
        encoder_out = np.transpose(encoder_out, (0, 2, 1))
        return self._decode_sequence(encoder_out[0], int(encoder_lens[0]))

    def _transcribe_guarded(self, samples: np.ndarray) -> tuple[list[str], list[float]]:
        """`transcribe`, with one retry when the result looks collapsed.

        The retry pads silence onto both ends, which is enough of a nudge to
        move the window off whatever the encoder trips over (head+tail
        recovers 77% of collapses; head alone 52%, tail alone 32%). Retry
        timestamps are shifted back over the head pad so they stay in region
        seconds. A genuinely silent region simply decodes to nothing twice —
        at RTF 0.02 that costs less than losing the words does.
        """
        tokens, timestamps = self.transcribe(samples)
        seconds = len(samples) / PARAKEET_RATE
        if len(tokens) >= _COLLAPSE_WORDS_PER_SECOND * seconds:
            return tokens, timestamps
        pad = np.zeros(int(_COLLAPSE_RETRY_PAD_S * PARAKEET_RATE), dtype=samples.dtype)
        retry_tokens, retry_timestamps = self.transcribe(np.concatenate([pad, samples, pad]))
        if len(retry_tokens) <= len(tokens):
            return tokens, timestamps
        return retry_tokens, [max(0.0, t - _COLLAPSE_RETRY_PAD_S) for t in retry_timestamps]

    def transcribe_text(self, samples: np.ndarray) -> str:
        tokens, _ = self._transcribe_guarded(samples)
        return _detokenize(tokens)

    def transcribe_words(self, samples: np.ndarray) -> list[Word]:
        tokens, timestamps = self._transcribe_guarded(samples)
        return _tokens_to_words(tokens, timestamps)


def _default_model_dir() -> str:
    """The staged weights (downloads on first use; offline-safe once staged by
    dev/fetch_parakeet_onnx.py). The dev/ script is imported by path — it is
    not part of the installed package (snaps ship weights as components and
    always pass ``--model``)."""
    import importlib.util

    fetch = Path(__file__).resolve().parents[4] / "dev" / "fetch_parakeet_onnx.py"
    spec = importlib.util.spec_from_file_location("fetch_parakeet_onnx", fetch)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return str(mod.stage(mod.default_model_dir()))


class ParakeetAdapter:
    """Parakeet TDT int8 behind ``SttService``; chunked-commit streaming."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        streaming: bool = False,
        stream_arm_s: float = SC_ARM_S,
        stream_silence_cut_s: float = SC_SILENCE_CUT_S,
        stream_force_cut_s: float = SC_FORCE_CUT_S,
        stream_partial_cadence_s: float = PARTIAL_CADENCE_S,
        stream_partial_tail_s: float = PARTIAL_TAIL_S,
    ) -> None:
        self._model_dir = model_dir
        self._streaming = streaming
        self._stream_arm_s = float(stream_arm_s)
        self._stream_silence_cut_s = float(stream_silence_cut_s)
        self._stream_force_cut_s = float(stream_force_cut_s)
        # 0 disables partials (committed text only, the pre-2026-08-28 shape);
        # 0 tail means the tick decodes the whole uncommitted window.
        self._stream_partial_cadence_s = float(stream_partial_cadence_s)
        self._stream_partial_tail_s = float(stream_partial_tail_s)
        if self._stream_arm_s <= 0:
            raise ValueError("stream_arm_s must be > 0")
        if self._stream_silence_cut_s <= 0:
            raise ValueError("stream_silence_cut_s must be > 0")
        if self._stream_force_cut_s <= 0:
            raise ValueError("stream_force_cut_s must be > 0")
        if self._stream_partial_cadence_s < 0:
            raise ValueError("stream_partial_cadence_s must be >= 0 (0 disables partials)")
        if self._stream_partial_tail_s < 0:
            raise ValueError("stream_partial_tail_s must be >= 0 (0 = whole window)")
        self._model: _ParakeetOnnx | None = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        return self._streaming

    @property
    def candidate(self) -> Candidate:
        label = (
            os.path.basename(self._model_dir.rstrip("/"))
            if self._model_dir
            else "parakeet-tdt-0.6b-v3-int8"
        )
        return Candidate(
            model=label,
            engine="onnxruntime-cpu",
            streaming_strategy="chunked-commit" if self._streaming else "commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        label = self.candidate.model
        return Capabilities(
            models=(label,),
            languages=LANGUAGES,
            input_formats=(PARAKEET_FORMAT,),
            punctuation=True,  # Parakeet v3 emits punctuation + capitalisation
            translation=False,
        )

    async def _load_model(self) -> _ParakeetOnnx:
        async with self._model_lock:
            if self._model is None:
                model_dir = self._model_dir or await asyncio.to_thread(_default_model_dir)
                self._model = await asyncio.to_thread(_ParakeetOnnx, model_dir)
        return self._model

    async def unload(self) -> None:
        """Release the ONNX sessions (idle-unload, T27). Idempotent."""
        import gc

        async with self._model_lock:
            self._model = None
        gc.collect()

    async def _load_model_with_heartbeat(self, emit: EventSink) -> _ParakeetOnnx:
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        return await load

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        # Audio-push invariant: the client owns capture + conversion; we
        # advertise the accepted format and reject mismatches, never resample.
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != PARAKEET_RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {PARAKEET_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            model = await self._load_model_with_heartbeat(emit)
            # Ready BEFORE pulling audio — the client gates on it
            # (docs/architecture/ie115-lifecycle.md §3A).
            await emit(TranscriptionProgress(phase=PHASE_READY))

            if self._streaming:
                await self._run_streaming_session(model, audio, emit)
                return

            buffered = bytearray()
            seconds_since_progress = 0.0
            async for chunk in audio:
                buffered.extend(chunk.data)
                seconds_since_progress += chunk.duration_seconds
                if seconds_since_progress >= _PROGRESS_INTERVAL_SECONDS:
                    seconds_since_progress = 0.0
                    await emit(TranscriptionProgress())

            if not buffered:
                await emit(TranscriptionDone(text=""))
                return

            samples = np.frombuffer(bytes(buffered), dtype=np.int16).astype(np.float32) / 32768.0
            text = await asyncio.to_thread(model.transcribe_text, samples)
            # Batch mode is degenerate streaming (I7): one committed final.
            await emit(TranscriptionFinal(text=text, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    async def _run_streaming_session(
        self,
        model: _ParakeetOnnx,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        """Chunked commit (008 US3): SilenceCut watches for pause/force cuts;
        each finalized chunk is decoded once and committed wholesale. Between
        cuts the loop re-decodes the uncommitted window for unstable display
        text, so the screen is not blank until the first cut; it never touches
        the committed transcript. The shared loop enforces I1–I7 and returns
        exactly the concatenation of committed text for the terminal done."""
        from myna.testbed.streaming.loop import run_streaming_loop
        from myna.testbed.streaming.strategies import SilenceCut

        def decode(samples: np.ndarray, offset: float) -> Hypothesis:
            words = model.transcribe_words(samples)
            return Hypothesis(words=[Word(w.text, w.start + offset, w.end + offset) for w in words])

        transcript = await run_streaming_loop(
            audio,
            emit,
            decode,
            SilenceCut(
                arm_seconds=self._stream_arm_s,
                silence_cut_seconds=self._stream_silence_cut_s,
                force_cut_seconds=self._stream_force_cut_s,
            ),
            cadence_seconds=_PROGRESS_INTERVAL_SECONDS,  # liveness tick only
            # The force cut is the memory bound (I6) in chunked mode.
            window_cap_seconds=self._stream_force_cut_s + 5.0,
            overlap_seconds=1.0,  # murmure CHUNK_FORCED_OVERLAP_SECS
            partial_cadence_seconds=self._stream_partial_cadence_s or None,
            partial_tail_seconds=self._stream_partial_tail_s or None,
        )
        await emit(TranscriptionDone(text=transcript))
