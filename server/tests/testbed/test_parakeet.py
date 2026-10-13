"""Parakeet adapter units (008 US3) — model-free helpers only.

The decode port itself is exercised end-to-end by myna-bench bench against the
staged int8 weights (671 MB — not a unit-test fixture); here we pin the pure
text/vocab mechanics the emission loop depends on (I2 verbatim concat) and the
session dispatch paths (batch I7, streaming strategy wiring).
"""

from __future__ import annotations

import asyncio
import logging
import os
import sys
from types import SimpleNamespace

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from test_emission_invariants import assert_batch_degenerate

from myna.core import (
    PHASE_READY,
    WARNING_GPU_UNAVAILABLE,
    WARNING_MEMORY_PRESSURE,
    AudioFormat,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
)
from myna.server.cli import build_adapter, build_parser
from myna.server.lifecycle import MemoryPressureMonitor
from myna.testbed.keyword_bias import KeywordBias, keyword_bias
from myna.testbed.parakeet import (
    _COLLAPSE_RETRY_PAD_S,
    BATCH_WINDOW_CAP_S,
    CUDA_PROVIDER,
    FP32_ENCODER_FILE,
    FP32_JOINT_FILE,
    INT8_ENCODER_FILE,
    INT8_JOINT_FILE,
    PARAKEET_RATE,
    PARTIAL_CADENCE_S,
    ParakeetAdapter,
    _detokenize,
    _load_vocab,
    _ParakeetOnnx,
    _require_cuda,
    _tokens_to_words,
    encoder_run_options,
    model_files,
)
from myna.testbed.streaming.coverage import RETRY_PADS
from myna.testbed.streaming.strategies import SilenceCut, Word

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


# ─── Shared helpers ──────────────────────────────────────────────────────────


class _FakeParakeetModel:
    """Minimal stub for _ParakeetOnnx: returns a fixed transcript without
    loading any ONNX weights."""

    def __init__(self, text: str = "hello world"):
        self._text = text
        self.calls: list[int] = []  # lengths of sample arrays passed in
        self.biases: list[object] = []
        # T10: run_session drives the real MemoryPressureMonitor lifecycle
        # (begin_session/observe_decode) against whatever model it holds; the
        # fake needs a real one so that path is exercised, not stubbed out.
        self.pressure_monitor = MemoryPressureMonitor()
        self.gpu_unavailable = False
        self.device = "cpu"

    def transcribe_words(self, samples, bias=None) -> list[Word]:
        self.calls.append(len(samples))
        self.biases.append(bias)
        return [Word(f" {w}", i, i + 1) for i, w in enumerate(self._text.split())]


async def pcm_audio(seconds: float, chunk_s: float = 0.5):
    for _ in range(int(seconds / chunk_s)):
        yield PcmChunk(data=b"\x01\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run_session(adapter, audio_seconds: float = 2.0, fmt=FORMAT):
    events = []

    async def emit(e):
        events.append(e)

    cfg = SessionConfig(audio_format=fmt, language="en")
    await adapter.run_session(cfg, pcm_audio(audio_seconds), emit)
    return events


def test_detokenize_strips_leading_and_pre_punctuation_spaces():
    # ▁→space already applied at vocab load; murmure's DECODE_SPACE_RE parity.
    tokens = [" Hello", ",", " world", "!", " It", " is", " me", "."]
    assert _detokenize(tokens) == "Hello, world! It is me."


def test_detokenize_empty():
    assert _detokenize([]) == ""


def test_tokens_to_words_groups_subwords_with_natural_spacing():
    tokens = [" Hel", "lo", " world", "!"]
    timestamps = [0.0, 0.08, 0.16, 0.32]
    words = _tokens_to_words(tokens, timestamps)
    # Punctuation attaches to its word (whisper word-text parity: " world!").
    assert [w.text for w in words] == [" Hello", " world!"]
    # Word spans run token-start to next-token-start; the last gets one frame.
    assert words[0].start == 0.0 and words[0].end == 0.16
    assert words[-1].end == 0.32 + 0.08


def test_tokens_to_words_first_token_without_space_still_opens_a_word():
    words = _tokens_to_words(["Hel", "lo", " again"], [0.0, 0.08, 0.16])
    assert [w.text for w in words] == ["Hello", " again"]


def test_load_vocab(tmp_path):
    vocab_file = tmp_path / "vocab.txt"
    vocab_file.write_text("<unk> 0\n▁hello 5\nworld 9\n<blk> 10\n", encoding="utf-8")
    vocab, blank = _load_vocab(str(tmp_path))
    assert blank == 10
    assert vocab[5] == " hello"  # ▁ becomes a literal space
    assert vocab[9] == "world"
    assert len(vocab) == 11


async def test_streaming_cut_constants_are_configurable(monkeypatch):
    """The snap exposes the SilenceCut knobs; the adapter must wire them into
    the strategy instead of always using murmure's defaults."""
    adapter = ParakeetAdapter(
        streaming=True,
        stream_arm_s=2.5,
        stream_silence_cut_s=0.25,
        stream_force_cut_s=7.0,
    )
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured["strategy"] = strategy
        captured["kwargs"] = kwargs
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    async def emit(_event):
        pass

    await adapter._run_streaming_session(object(), empty_audio(), emit)

    strategy = captured["strategy"]
    assert isinstance(strategy, SilenceCut)
    assert strategy._arm == 2.5
    assert strategy._silence_cut == 0.25
    assert strategy._force_cut == 7.0
    assert captured["kwargs"]["window_cap_seconds"] == 12.0


async def test_streaming_decode_schedules_pressure_warning_across_the_thread(monkeypatch):
    """T10 on the streaming path: `decode` runs off-loop (run_streaming_loop's
    dedicated worker thread), so the warning has to cross back via
    run_coroutine_threadsafe rather than a direct await. Captures the real
    `decode` closure and calls it as the worker thread would, then lets the
    event loop run one tick to pick up the scheduled emit."""
    from myna.server import lifecycle

    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: 800 * 1024**2)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)

    adapter = ParakeetAdapter(streaming=True)
    model = _FakeParakeetModel("hello world")
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured["decode"] = decode
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    events = []

    async def emit(event):
        events.append(event)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    await adapter._run_streaming_session(model, empty_audio(), emit)

    decode = captured["decode"]
    hyp = decode(np.zeros(1600, dtype=np.float32), 0.0)
    assert hyp.words, "sanity: the captured decode closure actually ran the fake model"
    await asyncio.sleep(0.01)  # let run_coroutine_threadsafe's scheduled coroutine execute

    warnings = [(e.warning, e.warning_code) for e in events if getattr(e, "warning", None)]
    assert warnings == [(lifecycle.MEMORY_PRESSURE_MESSAGE, WARNING_MEMORY_PRESSURE)]


def test_streaming_cut_constants_must_be_positive():
    with pytest.raises(ValueError, match="stream_arm_s"):
        ParakeetAdapter(stream_arm_s=0)
    with pytest.raises(ValueError, match="stream_silence_cut_s"):
        ParakeetAdapter(stream_silence_cut_s=0)
    with pytest.raises(ValueError, match="stream_force_cut_s"):
        ParakeetAdapter(stream_force_cut_s=0)


@pytest.mark.parametrize("force", [0.8, 1.0, 1.3])
def test_a_force_cut_must_leave_a_decode_past_the_overlap(force):
    with pytest.raises(ValueError, match="stream_force_cut_s must be > 1.3"):
        ParakeetAdapter(stream_force_cut_s=force)


def test_the_shortest_force_cut_past_the_overlap_is_accepted():
    assert ParakeetAdapter(stream_force_cut_s=1.31)._stream_force_cut_s == 1.31


@pytest.mark.asyncio
async def test_cli_refuses_a_force_cut_within_the_overlap_before_serving(tmp_path):
    from myna.server.cli import serve

    args = build_parser().parse_args(
        ["--socket", str(tmp_path / "s.sock"), "--adapter", "parakeet", "--streaming"]
        + ["--stream-force-cut-s", "1.0"]
    )
    with pytest.raises(SystemExit, match="stream_force_cut_s must be > 1.3"):
        await asyncio.wait_for(serve(args), timeout=10.0)
    assert not (tmp_path / "s.sock").exists()


def test_cli_wires_streaming_cut_constants():
    args = build_parser().parse_args(
        [
            "--socket",
            "/tmp/s.sock",
            "--adapter",
            "parakeet",
            "--streaming",
            "--stream-arm-s",
            "2.5",
            "--stream-silence-cut-s",
            "0.25",
            "--stream-force-cut-s",
            "7.0",
        ]
    )
    adapter = build_adapter(args)
    assert adapter._stream_arm_s == 2.5
    assert adapter._stream_silence_cut_s == 0.25
    assert adapter._stream_force_cut_s == 7.0


# ─── Session dispatch tests ──────────────────────────────────────────────────
#
# These exercise the run_session hot path without loading the 671 MB ONNX
# weights by pre-loading a _FakeParakeetModel stub on the adapter.


@pytest.mark.asyncio
async def test_batch_session_emits_complete_transcript_and_satisfies_i7():
    """I7: batch mode = one committed final carrying the full transcript,
    followed by TranscriptionDone.  Verifies the buffer-then-decode dispatch
    path and that no audio is silently discarded."""
    adapter = ParakeetAdapter(streaming=False)
    model = _FakeParakeetModel("he had never been father lover husband friend")
    adapter._model = model

    events = await run_session(adapter, audio_seconds=2.0)

    assert_batch_degenerate(events)
    done = events[-1]
    assert done.text == "he had never been father lover husband friend"
    assert model.calls, "the model was never called"


@pytest.mark.asyncio
async def test_batch_session_decodes_a_short_utterance_whole():
    adapter = ParakeetAdapter(streaming=False)
    model = _FakeParakeetModel("hello")
    adapter._model = model

    await run_session(adapter, audio_seconds=20.0)

    assert model.calls == [20 * PARAKEET_RATE]


@pytest.mark.asyncio
async def test_batch_session_bounds_every_decode_on_a_long_session():
    """One pass over a whole 5 minute clip peaked at 3.9 GB RSS."""
    adapter = ParakeetAdapter(streaming=False)
    model = _FakeParakeetModel("hello")
    adapter._model = model

    events = await run_session(adapter, audio_seconds=300.0)

    assert_batch_degenerate(events)
    assert len(model.calls) > 1
    assert max(model.calls) <= BATCH_WINDOW_CAP_S * PARAKEET_RATE
    assert sum(model.calls) >= 300 * PARAKEET_RATE


@pytest.mark.asyncio
async def test_batch_session_surfaces_memory_pressure_warning_once(monkeypatch):
    """T10: run_session wires MemoryPressureMonitor's signal onto
    transcription.progress -- and re-arms the debounce each session, exactly
    like LifecycleService's own idle-release re-arm."""
    from myna.server import lifecycle

    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: 800 * 1024**2)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)

    adapter = ParakeetAdapter(streaming=False)
    model = _FakeParakeetModel("hello")
    adapter._model = model

    events = await run_session(adapter, audio_seconds=2.0)
    warnings = [(e.warning, e.warning_code) for e in events if getattr(e, "warning", None)]
    assert warnings == [(lifecycle.MEMORY_PRESSURE_MESSAGE, WARNING_MEMORY_PRESSURE)]

    # A second session on the same (still undersized) model warns again.
    events2 = await run_session(adapter, audio_seconds=2.0)
    warnings2 = [e.warning for e in events2 if getattr(e, "warning", None)]
    assert warnings2 == [lifecycle.MEMORY_PRESSURE_MESSAGE]


@pytest.mark.asyncio
async def test_batch_session_no_warning_on_a_healthy_machine(monkeypatch):
    from myna.server import lifecycle

    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: None)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)

    adapter = ParakeetAdapter(streaming=False)
    adapter._model = _FakeParakeetModel("hello")

    events = await run_session(adapter, audio_seconds=2.0)
    assert not any(getattr(e, "warning", None) for e in events)


@pytest.mark.asyncio
async def test_batch_session_empty_audio_emits_empty_done():
    adapter = ParakeetAdapter(streaming=False)
    adapter._model = _FakeParakeetModel("never called")

    events = await run_session(adapter, audio_seconds=0.0)

    done = events[-1]
    assert isinstance(done, TranscriptionDone)
    assert done.text == ""


@pytest.mark.asyncio
async def test_batch_session_off_format_rejected():
    adapter = ParakeetAdapter(streaming=False)
    adapter._model = _FakeParakeetModel("never called")
    bad = AudioFormat(sample_rate_hz=8_000, channels=1, sample_width_bytes=2)

    events = await run_session(adapter, fmt=bad)

    assert type(events[0]).__name__ == "TranscriptionError"
    assert events[0].code == "unsupported_audio_format"


# ─── Collapse guard (2026-08-28) ─────────────────────────────────────────────
#
# The encoder returns blank-at-every-frame for particular window lengths; a
# retry with the window nudged by silence recovers ~3/4 of them. These build a
# _ParakeetOnnx without __init__ so no ONNX session is created.


def _tone(seconds: float, rms: float = 0.05, seed: int = 5) -> np.ndarray:
    """Stationary room tone: loud, and no more modulated than a fan."""
    rng = np.random.default_rng(seed)
    samples = rng.standard_normal(int(seconds * PARAKEET_RATE)).astype(np.float32)
    return samples * (rms / np.sqrt(np.mean(samples * samples)))


def _speech(seconds: float, rms: float = 0.05, floor: float = 0.006) -> np.ndarray:
    """Speech-shaped: 0.4 s bursts over a 0.15 s inter-word floor."""
    rng = np.random.default_rng(3)
    out: list[np.ndarray] = []
    while sum(len(c) for c in out) < seconds * PARAKEET_RATE:
        for span, level in ((0.4, rms), (0.15, floor)):
            block = rng.standard_normal(int(span * PARAKEET_RATE)).astype(np.float32)
            out.append(block * (level / np.sqrt(np.mean(block * block))))
    return np.concatenate(out)[: int(seconds * PARAKEET_RATE)]


def _bare_model(script):
    """A _ParakeetOnnx whose `transcribe` is `script(samples) -> (tokens, ts)`,
    recording the sample counts it was called with."""
    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.calls = []

    def transcribe(samples, *, bias=None):
        model.calls.append(len(samples))
        return script(len(samples), len(model.calls))

    model.transcribe = transcribe
    return model


def test_plausible_decode_is_not_retried():
    """A healthy chunk must cost exactly one decode — the retry is for the
    collapse, not a tax on every commit."""
    model = _bare_model(lambda n, call: ([" one", " two", " three"], [0.0, 0.5, 1.0]))

    tokens, _ = model._transcribe_guarded(_speech(2.0))

    assert tokens == [" one", " two", " three"]
    assert len(model.calls) == 1, "healthy decode must not pay for a retry"


def test_collapsed_decode_is_retried_with_padding_on_both_ends():
    """Zero words out of a second and a half of speech is the collapse
    signature; a region long enough to hold an UNTRANSCRIBED_GAP_S hole goes
    round the pad ladder instead."""

    def script(n, call):
        if call == 1:
            return [], []
        return ([" recovered", " text"], [0.3, 0.8])

    model = _bare_model(script)
    tokens, timestamps = model._transcribe_guarded(_speech(1.5))

    assert len(model.calls) == 2, "a collapsed decode must be retried once"
    pad_samples = int(_COLLAPSE_RETRY_PAD_S * PARAKEET_RATE)
    assert model.calls[1] == model.calls[0] + 2 * pad_samples, "pad goes on both ends"
    assert tokens == [" recovered", " text"]
    # Times come back in region seconds, with the head pad taken off again.
    assert timestamps == [pytest.approx(0.1), pytest.approx(0.6)]


def test_retry_that_does_not_help_keeps_the_original_decode():
    """The nudge recovers about three quarters of collapses; when it does not,
    the guard must not make things worse or loop."""

    def script(n, call):
        return ([" solitary"], [1.2]) if call == 1 else ([], [])

    model = _bare_model(script)
    tokens, timestamps = model._transcribe_guarded(_speech(2.3))

    assert len(model.calls) == 2, "exactly one retry, never a loop"
    assert tokens == [" solitary"] and timestamps == [1.2]


def test_a_partial_collapse_is_retried_even_though_the_region_looks_plausible():
    """The words-per-second check passes on a decode that transcribed most of
    the region and went blank over seven seconds of it - the partial collapse
    measured on the int8 encoder (parakeet.py and streaming.coverage)."""

    def script(n, call):
        if call == 1:
            return ([f" w{i}" for i in range(20)], [0.1 * i for i in range(20)])
        return ([f" w{i}" for i in range(40)], [0.3 * i for i in range(40)])

    model = _bare_model(script)
    tokens, _ = model._transcribe_guarded(_speech(13.0))

    assert len(model.calls) == 2, "a partial collapse must be re-decoded"
    assert len(tokens) == 40


def test_the_retry_ladder_keeps_the_nudge_that_leaves_the_least_untranscribed():
    """Nudging is a lottery per window: 0.2 s recovered 11 of 13 measured
    collapses and 0.3 s the two it lost, so the ladder tries both."""
    covered = ([f" w{i}" for i in range(40)], [0.3 * i for i in range(40)])

    def script(n, call):
        return ([], []) if call <= 2 else covered

    model = _bare_model(script)
    tokens, _ = model._transcribe_guarded(_speech(13.0))

    pads = [int(pad * PARAKEET_RATE) for pad in RETRY_PADS]
    assert [n - model.calls[0] for n in model.calls[1:]] == [2 * pad for pad in pads]
    assert tokens == covered[0]


def test_a_collapse_the_ladder_cannot_recover_keeps_the_best_attempt():
    def script(n, call):
        return ([" one"], [0.2]) if call == 1 else ([], [])

    model = _bare_model(script)
    tokens, timestamps = model._transcribe_guarded(_speech(13.0))

    assert len(model.calls) == 1 + len(RETRY_PADS), "the ladder runs once, not forever"
    assert (tokens, timestamps) == ([" one"], [0.2])


def test_a_healthy_decode_of_speech_is_not_retried():
    model = _bare_model(
        lambda n, call: ([f" w{i}" for i in range(26)], [0.5 * i for i in range(26)])
    )

    model._transcribe_guarded(_speech(13.0))

    assert len(model.calls) == 1


def test_short_region_is_judged_on_its_own_length():
    """The threshold is words per second, so a 0.5 s region yielding one word
    is plausible and must not be retried."""
    model = _bare_model(lambda n, call: ([" hi"], [0.0]))

    model._transcribe_guarded(_speech(0.5))

    assert len(model.calls) == 1


# ─── Room tone costs one decode (2026-09-18) ─────────────────────────────────


@pytest.mark.parametrize("rms", [0.005, 0.008, 0.02])
def test_token_free_room_tone_is_decoded_once(rms):
    """The shipped streaming config re-decodes the whole uncommitted window
    twice a second, so a hold-to-talk window nobody has spoken into yet must
    not pay for a retry ladder looking for words that are not there."""
    model = _bare_model(lambda n, call: ([], []))

    model._transcribe_guarded(_tone(60.0, rms))

    assert len(model.calls) == 1


def test_the_retried_decode_count_is_bounded():
    model = _bare_model(lambda n, call: ([], []))

    model._transcribe_guarded(_speech(60.0))

    assert len(model.calls) == 1 + len(RETRY_PADS)


def test_a_gap_the_ladder_cannot_close_is_logged(caplog):
    """A persistent partial collapse is accepted, so it has to be visible -
    and the seconds it names are what an operator acts on."""
    model = _bare_model(lambda n, call: ([" one"], [0.2]))

    with caplog.at_level(logging.WARNING, logger="myna.testbed.parakeet"):
        model._transcribe_guarded(_speech(13.0))

    (record,) = [r for r in caplog.records if "untranscribed" in r.getMessage()]
    assert "12.8 s of 13.0 s" in record.getMessage()


def test_a_gap_the_ladder_closes_is_not_logged(caplog):
    covered = ([f" w{i}" for i in range(40)], [0.3 * i for i in range(40)])

    def script(n, call):
        return ([" one"], [0.2]) if call == 1 else covered

    model = _bare_model(script)
    with caplog.at_level(logging.WARNING, logger="myna.testbed.parakeet"):
        model._transcribe_guarded(_speech(13.0))

    assert not [r for r in caplog.records if "untranscribed" in r.getMessage()]


def test_a_pad_retry_times_its_tokens_inside_the_region():
    """A token the decoder placed in the tail pad is timed past the region
    end unless it is clamped, and coverage.py then reads it as coverage the
    region never had - and the streaming loop can hold the word past a cut."""
    model = _bare_model(lambda n, call: ([" early", " late"], [0.0, 10.45]))

    _tokens, timestamps = model._padded(_speech(10.0), 0.3)

    assert timestamps == [0.0, pytest.approx(10.0)]


# ─── Partial (unstable) emission wiring ──────────────────────────────────────


async def test_partial_dials_reach_the_loop(monkeypatch):
    adapter = ParakeetAdapter(
        streaming=True, stream_partial_cadence_s=0.25, stream_partial_tail_s=4.0
    )
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured.update(kwargs)
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    async def emit(_event):
        pass

    await adapter._run_streaming_session(object(), empty_audio(), emit)

    assert captured["partial_cadence_seconds"] == 0.25
    assert captured["partial_tail_seconds"] == 4.0


async def test_zero_tail_means_the_whole_window(monkeypatch):
    """0 must reach the loop as None (whole window), not as a 0-second cap."""
    adapter = ParakeetAdapter(streaming=True, stream_partial_tail_s=0)
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured.update(kwargs)
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    async def emit(_event):
        pass

    await adapter._run_streaming_session(object(), empty_audio(), emit)

    assert captured["partial_tail_seconds"] is None


async def test_zero_cadence_disables_partials(monkeypatch):
    """0 is a real setting, not a missing one: it restores commit-only output."""
    adapter = ParakeetAdapter(streaming=True, stream_partial_cadence_s=0)
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured.update(kwargs)
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    async def emit(_event):
        pass

    await adapter._run_streaming_session(object(), empty_audio(), emit)

    assert captured["partial_cadence_seconds"] is None


def test_partial_dials_are_validated():
    with pytest.raises(ValueError, match="stream_partial_cadence_s"):
        ParakeetAdapter(stream_partial_cadence_s=-1)
    with pytest.raises(ValueError, match="stream_partial_tail_s"):
        ParakeetAdapter(stream_partial_tail_s=-1)
    # 0 is legal on both: cadence 0 turns partials off, tail 0 means the whole
    # uncommitted window.
    ParakeetAdapter(stream_partial_cadence_s=0, stream_partial_tail_s=0)


def test_cli_wires_partial_dials_including_an_explicit_zero():
    base = ["--socket", "/tmp/s.sock", "--adapter", "parakeet", "--streaming"]
    adapter = build_adapter(build_parser().parse_args(base))
    assert adapter._stream_partial_cadence_s == PARTIAL_CADENCE_S
    assert adapter._stream_partial_tail_s == 0.0, "the tick decodes the whole window by default"

    off = build_adapter(build_parser().parse_args(base + ["--stream-partial-cadence-s", "0"]))
    assert off._stream_partial_cadence_s == 0.0, "an explicit 0 must survive the default"


# Precision and device (the nvidia-gpu engine). A model dir carries one export,
# told apart by its decoder_joint; the GPU path refuses int8 graphs, refuses to
# serve when ORT fell back to the CPU, and runs the joint unbound.


def _touch(directory, *names):
    for name in names:
        (directory / name).write_bytes(b"")


def test_model_files_reads_the_precision_off_the_decoder_joint(tmp_path):
    for precision, encoder, joint in (
        ("fp32", FP32_ENCODER_FILE, FP32_JOINT_FILE),
        ("int8", INT8_ENCODER_FILE, INT8_JOINT_FILE),
    ):
        directory = tmp_path / precision
        directory.mkdir()
        _touch(directory, encoder, joint)
        files = model_files(str(directory))
        assert files.precision == precision
        assert files.encoder == str(directory / encoder)
        assert files.decoder_joint == str(directory / joint)


def test_model_files_refuses_a_dir_with_two_precisions(tmp_path):
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE, INT8_ENCODER_FILE, INT8_JOINT_FILE)
    with pytest.raises(FileNotFoundError, match="exactly one decoder_joint"):
        model_files(str(tmp_path))


def test_model_files_refuses_a_dir_with_no_decoder_joint(tmp_path):
    _touch(tmp_path, FP32_ENCODER_FILE)
    with pytest.raises(FileNotFoundError, match="none"):
        model_files(str(tmp_path))


@pytest.mark.parametrize(
    ("joint", "encoder"),
    ((FP32_JOINT_FILE, FP32_ENCODER_FILE), (INT8_JOINT_FILE, INT8_ENCODER_FILE)),
)
def test_model_files_refuses_a_joint_without_its_encoder(tmp_path, joint, encoder):
    _touch(tmp_path, joint)
    with pytest.raises(FileNotFoundError, match=encoder):
        model_files(str(tmp_path))


def test_cuda_refuses_an_int8_export_before_loading_anything(tmp_path):
    _touch(tmp_path, INT8_ENCODER_FILE, INT8_JOINT_FILE)
    with pytest.raises(ValueError, match="cuda device needs fp32"):
        _ParakeetOnnx(str(tmp_path), device="cuda")


class _FakeSessionOptions:
    def __init__(self):
        self.log_severity_level = None
        self.graph_optimization_level = None
        self.intra_op_num_threads = None
        self.inter_op_num_threads = None


class _FakeGraphOptimizationLevel:
    ORT_ENABLE_ALL = "ORT_ENABLE_ALL"


class _FakeRunOptions:
    def __init__(self):
        self.entries = {}

    def add_run_config_entry(self, key, value):
        self.entries[key] = value


class _FakeSession:
    """Stand-in for onnxruntime.InferenceSession: get_providers() always
    returns provider names, never the (name, options) tuples a caller may
    have requested - real ORT behaves the same way."""

    def __init__(self, path, options, providers):
        self.path = path
        self.options = options
        self.requested_providers = list(providers)

    def get_providers(self):
        return [p[0] if isinstance(p, tuple) else p for p in self.requested_providers]

    def get_outputs(self):
        return [SimpleNamespace(shape=(1, 1, 1, 8))]


def test_cuda_init_builds_sessions_on_the_cuda_provider(tmp_path, monkeypatch):
    """Session dispatch: device="cuda" puts the encoder and decoder_joint on
    CUDA_PROVIDER, keeps the preprocessor on the CPU, and wires the
    arena-shrink run options; a session ORT could not put on CUDA would trip
    _require_cuda instead of silently serving from the CPU."""
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    fake_ort = SimpleNamespace(
        InferenceSession=_FakeSession,
        SessionOptions=_FakeSessionOptions,
        GraphOptimizationLevel=_FakeGraphOptimizationLevel,
        RunOptions=_FakeRunOptions,
    )
    monkeypatch.setitem(sys.modules, "onnxruntime", fake_ort)

    model = _ParakeetOnnx(str(tmp_path), device="cuda")

    assert model._preprocessor.requested_providers == ["CPUExecutionProvider"]
    assert model._encoder.requested_providers[0] == CUDA_PROVIDER
    assert model._decoder_joint.requested_providers[0] == CUDA_PROVIDER
    assert model._encoder_run_options.entries == {"memory.enable_memory_arena_shrinkage": "gpu:0"}


class _CudaLessSession(_FakeSession):
    """An InferenceSession on a machine whose CUDA provider cannot load (no
    libcuda.so.1, an old driver): ORT logs it and hands back a CPU session."""

    def get_providers(self):
        return ["CPUExecutionProvider"]


def _fake_ort(monkeypatch, session):
    monkeypatch.setitem(
        sys.modules,
        "onnxruntime",
        SimpleNamespace(
            InferenceSession=session,
            SessionOptions=_FakeSessionOptions,
            GraphOptimizationLevel=_FakeGraphOptimizationLevel,
            RunOptions=_FakeRunOptions,
        ),
    )


def test_cuda_init_serves_on_the_cpu_when_the_cuda_provider_cannot_load(tmp_path, monkeypatch):
    """#49: a GPU engine whose driver is missing failed every utterance. The
    encoder ORT built instead is a working CPU session: keep it, and put the
    joint on the CPU too, driven the CPU way."""
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, _CudaLessSession)

    model = _ParakeetOnnx(str(tmp_path), device="cuda")

    assert model.gpu_unavailable
    assert model.device == "cpu"
    assert model._decoder_joint.requested_providers == ["CPUExecutionProvider"]
    assert model._encoder_run_options is None


class _OrtFail(Exception):
    """onnxruntime_pybind11_state.Fail: derives from Exception alone."""


class _PascalSession(_FakeSession):
    """An InferenceSession on a GPU CUDA 13 has no kernels for (#56): the CUDA
    provider loads, then cublasCreate fails and session creation throws."""

    def __init__(self, path, options, providers):
        if CUDA_PROVIDER in providers:
            raise _OrtFail(
                "[ONNXRuntimeError] : 1 : FAIL : Exception during initialization: "
                "CUBLAS failure 8: the function requires an architectural feature "
                "absent from the device ; expr=cublasCreate(&cublas_handle_);"
            )
        super().__init__(path, options, providers)


def test_cuda_init_serves_on_the_cpu_when_the_cuda_provider_throws(tmp_path, monkeypatch):
    """#56: a GTX 1050 failed every session at the encoder's creation."""
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, _PascalSession)

    model = _ParakeetOnnx(str(tmp_path), device="cuda", encoder_threads=3)

    assert model.gpu_unavailable
    assert model._encoder.options.intra_op_num_threads == 3
    assert model.device == "cpu"
    assert model._encoder.requested_providers == ["CPUExecutionProvider"]
    assert model._decoder_joint.requested_providers == ["CPUExecutionProvider"]
    assert model._encoder_run_options is None


def test_an_encoder_that_fails_on_the_cpu_too_is_not_a_gpu_fault(tmp_path, monkeypatch):
    """A broken export must fail the load, not be served as a GPU fallback."""

    class _BrokenEncoder(_FakeSession):
        def __init__(self, path, options, providers):
            if path.endswith(FP32_ENCODER_FILE):
                raise _OrtFail("[ONNXRuntimeError] : 7 : INVALID_PROTOBUF")
            super().__init__(path, options, providers)

    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, _BrokenEncoder)

    with pytest.raises(_OrtFail, match="INVALID_PROTOBUF"):
        _ParakeetOnnx(str(tmp_path), device="cuda")


class _SmallGpuSession(_FakeSession):
    """A session on a GPU with too little memory for a long window: CUDA
    runs fail the way ORT's arena does (T1000, 4 GB), CPU runs work."""

    runs: list[tuple[str, str]] = []

    def run(self, outputs, feeds, run_options=None):
        on = "cuda" if CUDA_PROVIDER in self.requested_providers else "cpu"
        name = os.path.basename(self.path)
        self.runs.append((name, on))
        if name == "nemo128.onnx":
            return [np.zeros((1, 128, 8), dtype=np.float32), np.array([8], dtype=np.int64)]
        if on == "cuda":
            raise _OrtFail(
                "[ONNXRuntimeError] : 1 : FAIL : Non-zero status code returned while "
                "running Mul node. Name:'/pre_encode/conv/Mul_7' Status Message: "
                "bfc_arena.cc:358 Failed to allocate memory for requested buffer of size 72155136"
            )
        return [np.zeros((1, 1024, 2), dtype=np.float32), np.array([2], dtype=np.int64)]


def _small_gpu_model(tmp_path, monkeypatch, session=_SmallGpuSession, device="cuda"):
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, session)
    session.runs = []
    model = _ParakeetOnnx(str(tmp_path), device=device, encoder_threads=3)
    decoded = []
    monkeypatch.setattr(
        model, "_decode_sequence", lambda enc, n, bias=None: decoded.append(n) or (["x"], [0.0])
    )
    return model, decoded


def test_a_gpu_that_runs_out_of_memory_mid_session_finishes_on_the_cpu(tmp_path, monkeypatch):
    """A 4 GB card loads the fp32 encoder, then fails 35 s into a long
    dictation: the window is decoded on the CPU, and so is every later one."""
    model, decoded = _small_gpu_model(tmp_path, monkeypatch)
    assert model.device == "cuda" and not model.gpu_unavailable

    assert model.transcribe(np.zeros(16000, dtype=np.float32)) == (["x"], [0.0])

    assert decoded == [2]
    assert model.gpu_unavailable
    assert model.device == "cpu"
    assert model._encoder.requested_providers == ["CPUExecutionProvider"]
    assert model._encoder.options.intra_op_num_threads == 3
    assert model._decoder_joint.requested_providers == ["CPUExecutionProvider"]
    assert model._encoder_run_options is None
    model.transcribe(np.zeros(16000, dtype=np.float32))
    encoder_runs = [on for name, on in _SmallGpuSession.runs if name == FP32_ENCODER_FILE]
    assert encoder_runs == ["cuda", "cpu", "cpu"]


def test_a_run_that_fails_on_the_cpu_too_raises(tmp_path, monkeypatch):
    class _BrokenEverywhere(_SmallGpuSession):
        def run(self, outputs, feeds, run_options=None):
            if os.path.basename(self.path) == FP32_ENCODER_FILE:
                raise _OrtFail("[ONNXRuntimeError] : 2 : INVALID_ARGUMENT")
            return super().run(outputs, feeds, run_options)

    model, _ = _small_gpu_model(tmp_path, monkeypatch, _BrokenEverywhere)

    with pytest.raises(_OrtFail, match="INVALID_ARGUMENT"):
        model.transcribe(np.zeros(16000, dtype=np.float32))


def test_a_cpu_run_failure_is_not_retried(tmp_path, monkeypatch):
    class _BrokenCpu(_SmallGpuSession):
        def run(self, outputs, feeds, run_options=None):
            if os.path.basename(self.path) == FP32_ENCODER_FILE:
                self.runs.append((FP32_ENCODER_FILE, "cpu"))
                raise _OrtFail("[ONNXRuntimeError] : 2 : INVALID_ARGUMENT")
            return super().run(outputs, feeds, run_options)

    model, _ = _small_gpu_model(tmp_path, monkeypatch, _BrokenCpu, device="cpu")

    with pytest.raises(_OrtFail):
        model.transcribe(np.zeros(16000, dtype=np.float32))
    assert [r for r in _BrokenCpu.runs if r[0] == FP32_ENCODER_FILE] == [(FP32_ENCODER_FILE, "cpu")]


def test_a_loaded_cuda_provider_is_not_reported_unavailable(tmp_path, monkeypatch):
    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, _FakeSession)

    model = _ParakeetOnnx(str(tmp_path), device="cuda", encoder_threads=3)

    assert not model.gpu_unavailable
    assert model._encoder.options.intra_op_num_threads == 3
    assert model.device == "cuda"


def test_a_cuda_encoder_beside_a_cpu_joint_is_refused(tmp_path, monkeypatch):
    """No one device to drive both on: the joint's IOBinding is CPU-only."""

    class _JointOffCuda(_FakeSession):
        def get_providers(self):
            if self.path.endswith(FP32_JOINT_FILE):
                return ["CPUExecutionProvider"]
            return super().get_providers()

    _touch(tmp_path, FP32_ENCODER_FILE, FP32_JOINT_FILE)
    (tmp_path / "vocab.txt").write_text("<blk> 0\n", encoding="utf-8")
    _fake_ort(monkeypatch, _JointOffCuda)

    with pytest.raises(RuntimeError, match="decoder_joint: CUDAExecutionProvider is not active"):
        _ParakeetOnnx(str(tmp_path), device="cuda")


@pytest.mark.asyncio
@pytest.mark.parametrize("streaming", [False, True])
async def test_a_session_on_a_cpu_fallback_says_so_when_ready(streaming):
    from myna.server import lifecycle

    adapter = ParakeetAdapter(device="cuda", streaming=streaming)
    model = _FakeParakeetModel("hello")
    model.gpu_unavailable = True
    adapter._model = model

    events = await run_session(adapter, audio_seconds=1.0)

    warned = [e for e in events if getattr(e, "warning", None)]
    assert [(e.phase, e.warning_code) for e in warned] == [(PHASE_READY, WARNING_GPU_UNAVAILABLE)]
    assert warned[0].warning == lifecycle.GPU_UNAVAILABLE_MESSAGE
    assert isinstance(events[-1], TranscriptionDone)
    assert adapter.capabilities().runtime["execution_provider"] == "CPUExecutionProvider"


def test_cuda_arena_keeps_power_of_two_growth():
    """Streaming decodes a new window length on nearly every tick. An arena
    extended by exactly what each request needs never reuses a block across
    lengths and exhausted a 12 GB card 26 s into a session."""
    name, options = CUDA_PROVIDER
    assert name == "CUDAExecutionProvider"
    assert options.get("arena_extend_strategy", "kNextPowerOfTwo") == "kNextPowerOfTwo"


class _FakeOrt:
    class RunOptions:
        def __init__(self):
            self.entries = {}

        def add_run_config_entry(self, key, value):
            self.entries[key] = value


def test_cuda_encoder_runs_shrink_the_arena_and_cpu_runs_do_not():
    """Every window is a new length; without shrinking, the encoder's arena
    grew past a 12 GB card over one streaming benchmark."""
    options = encoder_run_options(_FakeOrt, "cuda")
    assert options.entries == {"memory.enable_memory_arena_shrinkage": "gpu:0"}
    assert encoder_run_options(_FakeOrt, "cpu") is None


class _Session:
    def __init__(self, providers):
        self._providers = providers

    def get_providers(self):
        return self._providers


def test_require_cuda_refuses_a_silent_cpu_fallback():
    _require_cuda(_Session(["CUDAExecutionProvider", "CPUExecutionProvider"]), "encoder")
    with pytest.raises(RuntimeError, match="encoder: CUDAExecutionProvider is not active"):
        _require_cuda(_Session(["CPUExecutionProvider"]), "encoder")
    with pytest.raises(RuntimeError, match="not active"):
        _require_cuda(_Session([]), "decoder_joint")


class _RecordingJoint:
    """decoder_joint stand-in: logits and states derived from the inputs, so a
    stale input shows up in the output."""

    def __init__(self, width):
        self.width = width
        self.feeds = []

    def run(self, names, feeds):
        self.feeds.append({k: np.array(v, copy=True) for k, v in feeds.items()})
        token = int(feeds["targets"][0, 0])
        frame = float(feeds["encoder_outputs"][0, 0, 0])
        logits = np.full((1, 1, 1, self.width), frame, dtype=np.float32)
        logits[..., token] = 100.0
        state = feeds["input_states_1"] + 1.0
        return [logits, state, feeds["input_states_2"] - 1.0]


def test_cuda_decode_step_feeds_the_current_inputs_and_fills_the_buffers():
    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.device = "cuda"
    model._joint_out_width = 8
    model._decoder_joint = _RecordingJoint(8)
    buffers = model._joint_buffers()
    assert buffers.io is None, "a CUDA session must not be driven through IOBinding"

    for step, (token, frame) in enumerate(((3, 0.25), (5, 0.75))):
        logits = model._decode_step(buffers, token, np.full(1024, frame, dtype=np.float32))
        fed = model._decoder_joint.feeds[step]
        assert fed["targets"][0, 0] == token
        assert fed["encoder_outputs"][0, 0, 0] == frame
        assert logits is buffers.logits
        assert logits[token] == 100.0 and logits[token - 1] == frame
        # The caller decides whether to adopt the new state; the step must not.
        assert np.all(buffers.state_in[0] == 0.0)
        assert np.all(buffers.state_out[0] == 1.0) and np.all(buffers.state_out[1] == -1.0)


def test_adapter_refuses_an_unknown_device():
    with pytest.raises(ValueError, match="cpu or cuda"):
        ParakeetAdapter(device="rocm")


def test_cli_wires_the_device_and_the_candidate_names_it():
    base = ["--socket", "/tmp/s.sock", "--adapter", "parakeet"]
    cpu = build_adapter(build_parser().parse_args(base))
    assert cpu._device == "cpu"
    assert cpu.candidate.engine == "onnxruntime-cpu"

    cuda = build_adapter(build_parser().parse_args(base + ["--device", "cuda"]))
    assert cuda._device == "cuda"
    assert cuda.candidate.engine == "onnxruntime-cuda"


# ─── Keyword biasing in the decode loop ──────────────────────────────────────
# A scripted decoder_joint: the logits at frame t are a fixed table, the
# duration head always says "advance one frame", so a test reads as the
# token race at each frame.

_KW_VOCAB = [" the", " cat", " Si", "ob", "han", "a", "x", "<blk>"]
_KW_BLANK = len(_KW_VOCAB) - 1
_KW = {token: i for i, token in enumerate(_KW_VOCAB)}


class _ScriptedJoint:
    def __init__(self, frames):
        self.frames = frames
        self.width = len(_KW_VOCAB) + 2

    def run(self, names, feeds):
        frame = int(feeds["encoder_outputs"][0, 0, 0])
        logits = np.full((1, 1, 1, self.width), -50.0, dtype=np.float32)
        logits[..., _KW_BLANK] = 0.0
        for token, score in self.frames[frame].items():
            logits[..., _KW[token]] = score
        logits[..., len(_KW_VOCAB) + 1] = 1.0  # duration bin 1
        state = feeds["input_states_1"]
        return [logits, state, feeds["input_states_2"]]


def _scripted_model(*frames):
    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.device = "cuda"  # unbound joint runs, so the fake needs no IOBinding
    model._vocab, model._blank_idx = list(_KW_VOCAB), _KW_BLANK
    model._vocab_size = len(_KW_VOCAB)
    model._decoder_joint = _ScriptedJoint(list(frames))
    model._joint_out_width = model._decoder_joint.width
    encodings = np.zeros((len(frames), 1024), dtype=np.float32)
    encodings[:, 0] = np.arange(len(frames))
    return model, encodings


def _decode(frames, keywords=()):
    model, encodings = _scripted_model(*frames)
    bias = keyword_bias(keywords, model._vocab, model._blank_idx) if keywords else None
    return model._decode_sequence(encodings, len(encodings), bias=bias)


def test_a_boosted_keyword_that_completes_is_kept():
    frames = [{" the": 5.0, " Si": 0.0}, {"a": 5.0, "ob": 0.0}, {"x": 5.0, "han": 0.0}]
    assert _decode(frames) == ([" the", "a", "x"], [0.0, 0.08, 0.16])
    tokens, timestamps = _decode(frames, ["Siobhan"])
    assert tokens == [" Si", "ob", "han"]
    assert timestamps == pytest.approx([0.0, 0.08, 0.16])


def test_an_unsure_near_miss_of_a_keyword_is_corrected_in_the_decode():
    # " Si" barely beats " the", so the word is unsure; "Sioba" is one edit
    # from the keyword.
    frames = [{" Si": 5.0, " the": 4.9}, {"ob": 5.0}, {"a": 5.0}]
    assert _decode(frames, ["Siobax"]) == ([" Siobax"], pytest.approx([0.0]))


def test_a_sure_near_miss_of_a_keyword_is_kept_in_the_decode():
    frames = [{" Si": 9.0, " the": 4.9}, {"ob": 9.0}, {"a": 9.0}]
    assert _decode(frames, ["Siobax"])[0] == [" Si", "ob", "a"]


def test_blank_is_never_overridden():
    frames = [{" Si": -1.0}, {"ob": -1.0}, {"han": -1.0}]
    assert _decode(frames, ["Siobhan"]) == ([], [])


def test_a_boosted_match_that_breaks_is_rolled_back():
    frames = [{" the": 5.0, " Si": 0.0}, {" cat": 40.0, "ob": 0.0}]
    assert _decode(frames, ["Siobhan"]) == _decode(frames)


def test_a_boosted_match_the_audio_ends_inside_is_rolled_back():
    frames = [{" the": 5.0, " Si": 0.0}, {"ob": 5.0}]
    tokens, timestamps = _decode(frames, ["Siobhan"])
    assert tokens == [" the", "ob"]
    assert timestamps == pytest.approx([0.0, 0.08])


def test_the_rollback_returns_to_the_first_boosted_token():
    frames = [
        {" cat": 5.0},
        {" the": 5.0, " Si": 0.0},
        {"a": 5.0, "ob": 0.0},
        {" cat": 40.0, "han": 0.0},
    ]
    assert _decode(frames, ["Siobhan"]) == _decode(frames)


def test_a_match_the_model_chose_itself_is_not_rolled_back():
    frames = [{" Si": 5.0}, {" cat": 40.0, "ob": 0.0}]
    assert _decode(frames, ["Siobhan"]) == ([" Si", " cat"], pytest.approx([0.0, 0.08]))


def test_a_rolled_back_frame_may_start_a_fresh_boost_later():
    frames = [
        {" the": 5.0, " Si": 0.0},
        {" cat": 40.0},
        {" the": 5.0, " Si": 0.0},
        {"a": 5.0, "ob": 0.0},
        {"x": 5.0, "han": 0.0},
    ]
    tokens, _ = _decode(frames, ["Siobhan"])
    assert tokens == [" the", " cat", " Si", "ob", "han"]


def test_keywords_the_audio_never_approaches_leave_the_decode_alone():
    frames = [{" the": 5.0}, {" cat": 5.0}, {}, {"a": 3.0, "x": 2.0}]
    assert _decode(frames, ["Siobhan"]) == _decode(frames)


def test_a_boosted_token_may_start_a_keyword_afresh_mid_match():
    frames = [
        {" Si": 5.0},
        {" the": 5.0, " Si": 0.0},
        {"a": 5.0, "ob": 0.0},
        {"x": 5.0, "han": 0.0},
    ]
    tokens, _ = _decode(frames, ["Siobhan"])
    assert tokens == [" Si", " Si", "ob", "han"]


class _RandomJoint:
    """decoder_joint stand-in whose logits are a pseudo-random function of
    the frame, the previous token and the decoder state, which counts the
    tokens adopted (+1 and +2 per token), so a decode that feeds the wrong
    state goes somewhere else. "han" never wins: Siobhan cannot complete."""

    def __init__(self, seed, durations, hot_frames=()):
        self.seed, self.durations, self.hot_frames = seed, durations, hot_frames
        self.width = len(_KW_VOCAB) + durations

    def run(self, names, feeds):
        frame = int(feeds["encoder_outputs"][0, 0, 0])
        key = (
            self.seed,
            frame,
            int(feeds["targets"][0, 0]),
            int(feeds["input_states_1"][0, 0, 0]),
            int(feeds["input_states_2"][0, 0, 0]),
        )
        rng = np.random.default_rng(abs(hash(key)))
        logits = rng.normal(0.0, 2.0, self.width).astype(np.float32)
        logits[_KW["han"]] = -50.0
        if self.durations:
            logits[len(_KW_VOCAB)] += 1.5  # duration 0 often: several tokens a frame
        if frame in self.hot_frames:
            logits[_KW_BLANK] = -50.0
            logits[len(_KW_VOCAB) :] = [0.0] + [-50.0] * (self.durations - 1)
        return [
            logits.reshape(1, 1, 1, -1),
            feeds["input_states_1"] + 1.0,
            feeds["input_states_2"] + 2.0,
        ]


def _random_decode(joint, frames, keywords):
    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.device = "cuda"
    model._vocab, model._blank_idx = list(_KW_VOCAB), _KW_BLANK
    model._vocab_size = len(_KW_VOCAB)
    model._decoder_joint = joint
    model._joint_out_width = joint.width
    encodings = np.zeros((frames, 1024), dtype=np.float32)
    encodings[:, 0] = np.arange(frames)
    bias = keyword_bias(keywords, model._vocab, model._blank_idx) if keywords else None
    return model._decode_sequence(encodings, frames, bias=bias)


@pytest.mark.parametrize("durations", [0, 3])
@pytest.mark.parametrize("seed", range(12))
def test_a_keyword_that_never_completes_never_changes_the_transcript(seed, durations, monkeypatch):
    # The decode's own property; correcting near misses afterwards is not.
    monkeypatch.setattr(KeywordBias, "correct", lambda self, tokens, times, _p: (tokens, times))
    joint = _RandomJoint(seed, durations, hot_frames=(0,) if seed % 2 else ())
    unbiased = _random_decode(joint, 24, ())
    assert unbiased[0], "sanity: the script emits tokens"
    assert _random_decode(joint, 24, ["Siobhan"]) == unbiased


def test_the_random_script_does_try_the_keyword():
    """Guards the property above against passing vacuously."""
    seen = []
    real_rerank = KeywordBias.rerank

    def rerank(self, state, logits, token):
        chosen = real_rerank(self, state, logits, token)
        seen.append(chosen != token)
        return chosen

    with pytest.MonkeyPatch.context() as mp:
        mp.setattr(KeywordBias, "rerank", rerank)
        for seed in range(12):
            _random_decode(_RandomJoint(seed, 3), 24, ["Siobhan"])
    assert sum(seen) >= 12


def test_guarded_retries_decode_with_the_same_bias():
    seen = []

    def transcribe(samples, *, bias=None):
        seen.append(bias)
        return [], []

    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.transcribe = transcribe
    bias = object()
    model.transcribe_words(_speech(1.5), bias=bias)
    assert len(seen) == 2 and all(b is bias for b in seen)


def test_the_parakeet_adapter_advertises_keywords():
    assert ParakeetAdapter().capabilities().keywords is True


async def _session_biases(adapter, keywords):
    model = _FakeParakeetModel("hello")
    model._vocab, model._blank_idx = list(_KW_VOCAB), _KW_BLANK
    adapter._model = model
    cfg = SessionConfig(audio_format=FORMAT, language="en", keywords=keywords)

    async def emit(_event):
        pass

    await adapter.run_session(cfg, pcm_audio(100.0), emit)
    return model.biases


@pytest.mark.parametrize("streaming", [False, True])
async def test_a_session_without_keywords_decodes_unbiased(streaming):
    biases = await _session_biases(ParakeetAdapter(streaming=streaming), ())
    assert biases and all(b is None for b in biases)


@pytest.mark.parametrize("streaming", [False, True])
async def test_a_session_builds_its_keyword_bias_once(streaming):
    biases = await _session_biases(ParakeetAdapter(streaming=streaming), ("Siobhan",))
    assert len(biases) > 1, "the session should span several decodes"
    assert isinstance(biases[0], KeywordBias)
    assert all(b is biases[0] for b in biases)
