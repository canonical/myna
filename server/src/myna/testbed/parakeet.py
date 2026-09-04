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
time-to-first-word from 17.7 s to ~2.1 s at the shipped 2.0 s cadence (perf
T04, 2026-08-29: the original 0.5 s cadence reached 0.6 s but ran the encoder
at up to 82.6% duty cycle and could fall behind real time entirely on
pause-free audio — see ``PARTIAL_CADENCE_S``). All emission invariants
(I1–I7) are enforced by the shared ``streaming.loop``.

Requires the ``parakeet`` extra: ``uv sync --extra parakeet``. Weights are
murmure's ``parakeet-tdt-0.6b-v3-int8`` bundle, staged by
``dev/parakeet/fetch_parakeet_onnx.py`` (pinned + sha256-verified) into the XDG cache;
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
import logging
import os
import re
import time
from collections.abc import AsyncIterator, Callable
from contextlib import nullcontext
from dataclasses import dataclass
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
from myna.server.lifecycle import MemoryPressureMonitor, sample_majflt
from myna.testbed.adapter import Candidate
from myna.testbed.harness import StreamingTelemetry
from myna.testbed.streaming.strategies import (
    SC_ARM_S,
    SC_FORCE_CUT_S,
    SC_SILENCE_CUT_S,
    Hypothesis,
    Word,
)

# Tracy zones (dev tooling only): `tracy_client` is built from source
# by hand and never present in a production
# install, so this is a permanent import-time branch, not a per-call one -
# `_zone` costs one nullcontext() when disabled, same order of magnitude as
# the `bench is not None` checks below.
try:
    from tracy_client import ScopedZone as _TracyZone

    _TRACY = True
except ImportError:
    _TRACY = False


def _zone(name: str):
    return _TracyZone(name) if _TRACY else nullcontext()


_log = logging.getLogger(__name__)


PARAKEET_RATE = 16_000
PARAKEET_FORMAT = AudioFormat(sample_rate_hz=PARAKEET_RATE, channels=1, sample_width_bytes=2)

# Optimized encoder variant (ratified 2026-08-31): the "maxstack" encoder
# (10-of-11 FFN requant + fused SiLU custom ops + export cleanups; built by
# dev/parakeet/build_maxstack_encoder.py) plus the custom-op kernel library it
# needs. Measured encode -13.3% on the same audio path, and since 2026-09-01 it
# is the only encoder the snap component ships
# (parakeet-snap/dev/download-models.sh) - the base export it derives from
# stays a build input.
#
# The base encoder is still selected for a dir that carries it and not the
# pair, which is what an unprocessed upstream bundle looks like: the model
# cache the maxstack build reads, and any other staging of the murmure
# release. MYNA_ORT_CUSTOM_OPS overrides the library location (dev tooling).
BASE_ENCODER_FILE = "encoder-model.int8.onnx"
MAXSTACK_ENCODER_FILE = "encoder-model.int8.maxstack.onnx"
QSILU_LIB_FILE = "libqsilu.so"


def encoder_variant(model_dir: str) -> tuple[str, str | None]:
    """(encoder path, custom-ops library path or None) for a model dir."""
    lib = os.environ.get("MYNA_ORT_CUSTOM_OPS") or os.path.join(model_dir, QSILU_LIB_FILE)
    maxstack = os.path.join(model_dir, MAXSTACK_ENCODER_FILE)
    if os.path.exists(maxstack) and os.path.exists(lib):
        return maxstack, lib
    base = os.path.join(model_dir, BASE_ENCODER_FILE)
    if not os.path.exists(base):
        # A shipped component has no base encoder to fall back to, so a lost
        # kernel library surfaces here rather than in ORT with a path it cannot
        # explain. Name the pair: the answer is to restore or rebuild it.
        raise FileNotFoundError(
            f"{model_dir} carries no loadable encoder: {MAXSTACK_ENCODER_FILE} needs "
            f"{QSILU_LIB_FILE} beside it (or MYNA_ORT_CUSTOM_OPS pointing at it), and "
            f"there is no {BASE_ENCODER_FILE} to fall back to"
        )
    env_lib = os.environ.get("MYNA_ORT_CUSTOM_OPS")
    return base, env_lib if env_lib and os.path.exists(env_lib) else None


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

# Unstable-partial dials. Was 0.5 s (reads as continuous without the preedit
# region thrashing) until perf T04 (2026-08-29) mapped display quality
# against measured encoder cost (T03's StreamingTelemetry) across the
# (cadence, tail) grid: at 0.5 s the encoder ran 56.5-82.6% duty cycle
# depending on how much the speaker paused, and on pause-free audio it could
# not sustain real time at all (91.2 s wall for 60 s of audio). Every cadence
# tested left `head_loss_rate` at 0.00 (the display never loses its head,
# unlike tail-capping — see below) and time-to-first-unstable scales
# ~linearly with cadence, so 2.0 s was chosen as the cheapest cadence that
# stayed comfortably real-time on the pause-free case (30.7% duty, no
# backlog) while keeping first-word latency at ~2.1 s — still ~8x faster
# than the 17.7 s pre-partial-tick baseline.
#
# The tail cap is off by default (0 = decode the whole uncommitted window).
# T04 measured it as a *worse* lever than cadence for the same duty-cycle
# savings: at every cadence tested, capping the tail bought further cost
# reduction only by making 29-94% of unstable updates structurally drop the
# display's head (not just revise it — see [`_chunked_partial`], which
# carries the evidence for why the obvious middle ground does not work).
PARTIAL_CADENCE_S = 2.0
PARTIAL_TAIL_S = 0.0

# Murmure's DECODE_SPACE_RE: strip the leading space and spaces before
# punctuation, keep word-boundary spaces. Tokens carry ▁→space already.
_DECODE_SPACE_RE = re.compile(r"\A\s|\s\B|(\s)\b")

# Stage span name -> elapsed seconds. Wired only by dev/parakeet/bench_parakeet.py
# (T01); production callers never pass one, so the hot path pays nothing
# beyond the branch (verified <1% overhead over 71 joint calls, 2026-08-28).
BenchSink = Callable[[str, float], None]


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

    Re-measured 2026-08-29 (perf T08) on the reference Ryzen AI 7 350 (4 Zen 5
    cores at 5.09 GHz + 4 Zen 5c cores at 3.51 GHz, SMT2, 16 logical CPUs): the
    encoder scales almost linearly through 4 threads pinned one-per-physical
    fast-core (2.94x at 12.46 s of audio, 2.35-2.70x at 2-5 s windows), then
    genuinely goes backwards past 4 on the *same four cores* via SMT (6 or 8
    threads sharing those 4 cores' execution units) - both slower (+15-20%
    wall time) and costlier in joules per utterance than stopping at 4. That
    part of the old comment's mechanism holds up. What does not hold up is the
    "half the visible CPUs" arithmetic: it produces 4 on this box only because
    16 // 2 happens to clear the cap, not because halving models SMT contention
    in general. Crossing into the 4 slower Zen 5c cores (2026-08-28's untested
    "8 threads, 1.26x" figure) was not re-verified: doing so requires pinning
    across two CPU frequencies, which the measurement guard hard-refuses
    because that placement is exactly the source of its own documented 1.45x
    variance risk.

    ``sched_getaffinity`` rather than ``cpu_count`` so a taskset or a cgroup
    cpuset is respected; halved and capped at four so a machine with more
    visible CPUs (via SMT or a larger cpuset) does not walk back into the
    regression measured above, and floored at one so a single-CPU affinity
    mask still gets a working thread count.
    """
    return max(1, min(4, len(os.sched_getaffinity(0)) // 2))


@dataclass
class _JointBuffers:
    """Per-utterance reusable IOBinding state for decoder_joint (T09). See
    `_ParakeetOnnx._joint_buffers`."""

    io: object  # onnxruntime.IOBinding, left untyped to avoid an import here
    encoder_step: np.ndarray  # (1, 1024, 1), written in place per step
    target: np.ndarray  # (1, 1) int32, written in place per step
    state_in: tuple[np.ndarray, np.ndarray]  # (2,1,640) each: current committed state
    state_out: tuple[np.ndarray, np.ndarray]  # (2,1,640) each: scratch for the new state
    logits: np.ndarray  # (vocab+durations,), a view into the reused output buffer


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
        encoder_opts = opts(encoder_threads or _encoder_threads())
        # See encoder_variant: the maxstack encoder's myna.QSiLU* custom ops
        # (dev/parakeet/qsilu/) need their kernel library registered on the session.
        encoder_path, custom_ops = encoder_variant(model_dir)
        if custom_ops:
            encoder_opts.register_custom_ops_library(custom_ops)
        _log.info(
            "parakeet encoder variant: %s",
            "maxstack (custom ops registered)" if custom_ops else "base",
        )
        self._encoder = ort.InferenceSession(encoder_path, encoder_opts, providers=providers)
        self._decoder_joint = ort.InferenceSession(
            os.path.join(model_dir, "decoder_joint-model.int8.onnx"),
            opts(1),
            providers=providers,
        )
        self._vocab, self._blank_idx = _load_vocab(model_dir)
        # Logits are vocab + TDT duration bins; the duration slice is the tail.
        self._vocab_size = len(self._vocab)
        # Static for this model (batch=1, one target, one encoder frame per
        # call) - read once rather than hardcoded so a different joint export
        # can't silently mismatch it. See _JointBuffers / _joint_buffers.
        self._joint_out_width = int(self._decoder_joint.get_outputs()[0].shape[-1])
        # T10: model-load time is exactly the "startup" the SPEC wants for the
        # cgroup-limit prediction -- this is the point a 794 MB encoder is
        # about to become resident, so it's the earliest place a too-small
        # limit is known. See myna.server.lifecycle.MemoryPressureMonitor.
        self.pressure_monitor = MemoryPressureMonitor()

    def _joint_buffers(self) -> _JointBuffers:
        """Fresh IOBinding + pre-allocated buffers for one `_decode_sequence`
        call.

        Per-call cost was attributed to session dispatch (dict construction,
        fresh numpy allocation for every input and output, a non-contiguous
        reshape+astype copy of the encoder frame) stacked on top of real
        kernel time. Building these
        buffers once per utterance and reusing them for every one of its
        ~71 steps removes the per-step allocation; the kernel time itself
        (DynamicQuantizeMatMul/DynamicQuantizeLSTM) is untouched and turned
        out to be most of the 323 us, not the small remainder this buys back.

        Built fresh per utterance rather than cached on `self` so two
        `_decode_sequence` calls in flight at once (concurrent sessions on
        one server process share one `_ParakeetOnnx`) get independent memory,
        exactly as safe as the plain `session.run()` calls this replaces -
        setup cost is 13.6 us/utterance, 0.19 us amortized per step, so
        rebuilding it every call is free next to what it buys.
        """
        import onnxruntime as ort

        encoder_step = np.zeros((1, 1024, 1), dtype=np.float32)
        target = np.zeros((1, 1), dtype=np.int32)
        target_length = np.array([1], dtype=np.int32)  # constant: one target
        state_in = (
            np.zeros((2, 1, 640), dtype=np.float32),
            np.zeros((2, 1, 640), dtype=np.float32),
        )
        state_out = (
            np.zeros((2, 1, 640), dtype=np.float32),
            np.zeros((2, 1, 640), dtype=np.float32),
        )
        out = np.zeros((1, 1, 1, self._joint_out_width), dtype=np.float32)

        io = self._decoder_joint.io_binding()
        io.bind_ortvalue_input("encoder_outputs", ort.OrtValue.ortvalue_from_numpy(encoder_step))
        io.bind_ortvalue_input("targets", ort.OrtValue.ortvalue_from_numpy(target))
        io.bind_ortvalue_input("target_length", ort.OrtValue.ortvalue_from_numpy(target_length))
        io.bind_ortvalue_input("input_states_1", ort.OrtValue.ortvalue_from_numpy(state_in[0]))
        io.bind_ortvalue_input("input_states_2", ort.OrtValue.ortvalue_from_numpy(state_in[1]))
        io.bind_ortvalue_output("outputs", ort.OrtValue.ortvalue_from_numpy(out))
        io.bind_ortvalue_output("output_states_1", ort.OrtValue.ortvalue_from_numpy(state_out[0]))
        io.bind_ortvalue_output("output_states_2", ort.OrtValue.ortvalue_from_numpy(state_out[1]))
        return _JointBuffers(
            io=io,
            encoder_step=encoder_step,
            target=target,
            state_in=state_in,
            state_out=state_out,
            logits=out.reshape(-1),
        )

    def _decode_step(
        self,
        buffers: _JointBuffers,
        prev_token: int,
        encoder_step: np.ndarray,  # [1024]
    ) -> np.ndarray:
        """One decoder_joint call via `buffers` (T09: was a fresh dict, fresh
        output allocation and a reshape+astype copy per call; see
        `_joint_buffers`). The returned array is a view into `buffers`'
        reused output - valid only until the next `_decode_step` call on the
        same buffers, which is exactly how `_decode_sequence` consumes it
        (argmax, then either adopt or discard `buffers.state_out` before
        looping). `buffers.state_in` is a separate buffer from
        `state_out` and is never touched by ORT, so "discard" is simply "do
        not copy" - no ping-pong swap needed for the blank-token branch.
        """
        buffers.encoder_step[0, :, 0] = encoder_step
        buffers.target[0, 0] = prev_token
        with _zone("joint"):
            self._decoder_joint.run_with_iobinding(buffers.io)
        return buffers.logits

    def _decode_sequence(
        self, encodings: np.ndarray, encodings_len: int, *, bench: BenchSink | None = None
    ) -> tuple[list[str], list[float]]:
        """Greedy TDT decode (murmure decode_sequence_greedy): argmax vocab
        token; on non-blank update the decoder state; the duration head skips
        t forward; a blank (or MAX_TOKENS_PER_STEP at one frame) advances 1.

        ``bench``, when given, separates the ``joint`` span (summed ONNX call
        time) from ``greedy`` (loop wall time minus joint) the way T01's
        harness needs — the loop's own argmax/control-flow overhead is
        otherwise invisible next to 71+ sequential ORT calls. It also reports
        the non-timing counts the harness wants alongside the spans:
        ``_frames`` (``encodings_len``) and ``_joint_calls``.
        """
        if bench is not None:
            bench("_frames", float(encodings_len))
        buffers = self._joint_buffers()
        tokens: list[str] = []
        timestamps: list[float] = []
        t = 0
        emitted_at_frame = 0
        prev_token = self._blank_idx
        joint_s = 0.0
        joint_calls = 0
        loop_t0 = time.perf_counter() if bench is not None else 0.0
        with _zone("decode_sequence"):
            while t < encodings_len:
                if bench is None:
                    logits = self._decode_step(buffers, prev_token, encodings[t])
                else:
                    step_t0 = time.perf_counter()
                    logits = self._decode_step(buffers, prev_token, encodings[t])
                    joint_s += time.perf_counter() - step_t0
                    joint_calls += 1
                vocab_logits = logits[: self._vocab_size]
                dur_logits = logits[self._vocab_size :]
                token = int(np.argmax(vocab_logits))
                if token != self._blank_idx:
                    buffers.state_in[0][...] = buffers.state_out[0]
                    buffers.state_in[1][...] = buffers.state_out[1]
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
        if bench is not None:
            bench("joint", joint_s)
            bench("greedy", (time.perf_counter() - loop_t0) - joint_s)
            bench("_joint_calls", float(joint_calls))
        return tokens, timestamps

    def transcribe(
        self, samples: np.ndarray, *, bench: BenchSink | None = None
    ) -> tuple[list[str], list[float]]:
        """float32 mono 16 kHz → (tokens, token timestamps in region seconds).

        ``bench(name, seconds)`` fires once per stage (``preprocess``,
        ``encode``, ``transpose``, then ``joint``/``greedy`` from
        ``_decode_sequence``) — dev/parakeet/bench_parakeet.py's hook (T01). ``None``
        by default and on every production call path.
        """
        with _zone("transcribe"):
            waveforms = samples.reshape(1, -1).astype(np.float32)
            waveforms_lens = np.array([samples.shape[0]], dtype=np.int64)
            t0 = time.perf_counter() if bench is not None else 0.0
            with _zone("preprocess"):
                features, features_lens = self._preprocessor.run(
                    ["features", "features_lens"],
                    {"waveforms": waveforms, "waveforms_lens": waveforms_lens},
                )
            if bench is not None:
                bench("preprocess", time.perf_counter() - t0)
                t0 = time.perf_counter()
            with _zone("encode"):
                encoder_out, encoder_lens = self._encoder.run(
                    ["outputs", "encoded_lengths"],
                    {"audio_signal": features, "length": features_lens},
                )
            if bench is not None:
                bench("encode", time.perf_counter() - t0)
                t0 = time.perf_counter()
            # [1, 1024, T] → [1, T, 1024]. Forced contiguous (T09): a plain
            # np.transpose is a strided view, so every one of the ~71 per-frame
            # slices `_decode_step` takes below would itself be a non-contiguous
            # copy source; one real copy here makes each of those a cheap
            # contiguous memcpy instead.
            with _zone("transpose"):
                encoder_out = np.ascontiguousarray(np.transpose(encoder_out, (0, 2, 1)))
            if bench is not None:
                bench("transpose", time.perf_counter() - t0)
            return self._decode_sequence(encoder_out[0], int(encoder_lens[0]), bench=bench)

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
    dev/parakeet/fetch_parakeet_onnx.py). The dev/ script is imported by path — it is
    not part of the installed package (snaps ship weights as components and
    always pass ``--model``)."""
    import importlib.util

    fetch = Path(__file__).resolve().parents[4] / "dev" / "parakeet" / "fetch_parakeet_onnx.py"
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
        stream_telemetry: StreamingTelemetry | None = None,
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
        # perf T03: None on every production call path (dev tooling only).
        self._stream_telemetry = stream_telemetry
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
            # T10: re-arm the once-per-session debounce. A fresh session on a
            # persistently undersized machine should still get told, the same
            # way LifecycleService itself re-arms idle-release on a fresh
            # session rather than staying silent forever after the first.
            model.pressure_monitor.begin_session()
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
            # T10: sample from this thread, straddling the whole decode -- ORT
            # aggregates page faults process-wide (RUSAGE_SELF), so a worker
            # thread's faults count here regardless.
            majflt_before = sample_majflt()
            text = await asyncio.to_thread(model.transcribe_text, samples)
            warning = model.pressure_monitor.observe_decode(majflt_before, sample_majflt())
            if warning is not None:
                await emit(TranscriptionProgress(warning=warning))
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

        # T10: `decode` runs on run_streaming_loop's dedicated worker thread
        # (not the event loop), so a detected warning can't be `await
        # emit(...)`ed directly from here -- it's handed to the loop via
        # run_coroutine_threadsafe, fire-and-forget (advisory, debounced to
        # once per session; nothing downstream depends on its timing).
        event_loop = asyncio.get_running_loop()

        def decode(samples: np.ndarray, offset: float) -> Hypothesis:
            majflt_before = sample_majflt()
            words = model.transcribe_words(samples)
            warning = model.pressure_monitor.observe_decode(majflt_before, sample_majflt())
            if warning is not None:
                progress = TranscriptionProgress(warning=warning)
                asyncio.run_coroutine_threadsafe(emit(progress), event_loop)
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
            telemetry=self._stream_telemetry,
        )
        await emit(TranscriptionDone(text=transcript))
