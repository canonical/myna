"""Parakeet adapter units (008 US3) — model-free helpers only.

The decode port itself is exercised end-to-end by dev/bench.py against the
staged int8 weights (671 MB — not a unit-test fixture); here we pin the pure
text/vocab mechanics the emission loop depends on (I2 verbatim concat) and the
session dispatch paths (batch I7, streaming strategy wiring).
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from test_emission_invariants import assert_batch_degenerate

from myna.core import (
    AudioFormat,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
)
from myna.server.cli import build_adapter, build_parser
from myna.testbed.parakeet import (
    _COLLAPSE_RETRY_PAD_S,
    PARAKEET_RATE,
    ParakeetAdapter,
    _detokenize,
    _load_vocab,
    _ParakeetOnnx,
    _tokens_to_words,
)
from myna.testbed.streaming.strategies import SilenceCut

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


# ─── Shared helpers ──────────────────────────────────────────────────────────


class _FakeParakeetModel:
    """Minimal stub for _ParakeetOnnx: returns a fixed transcript without
    loading any ONNX weights."""

    def __init__(self, text: str = "hello world"):
        self._text = text
        self.calls: list[int] = []  # lengths of sample arrays passed in

    def transcribe_text(self, samples) -> str:
        self.calls.append(len(samples))
        return self._text


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


def test_streaming_cut_constants_must_be_positive():
    with pytest.raises(ValueError, match="stream_arm_s"):
        ParakeetAdapter(stream_arm_s=0)
    with pytest.raises(ValueError, match="stream_silence_cut_s"):
        ParakeetAdapter(stream_silence_cut_s=0)
    with pytest.raises(ValueError, match="stream_force_cut_s"):
        ParakeetAdapter(stream_force_cut_s=0)


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
    assert model.calls, "model.transcribe_text was never called"


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


def _bare_model(script):
    """A _ParakeetOnnx whose `transcribe` is `script(samples) -> (tokens, ts)`,
    recording the sample counts it was called with."""
    model = _ParakeetOnnx.__new__(_ParakeetOnnx)
    model.calls = []

    def transcribe(samples):
        model.calls.append(len(samples))
        return script(len(samples), len(model.calls))

    model.transcribe = transcribe
    return model


def test_plausible_decode_is_not_retried():
    """A healthy chunk must cost exactly one decode — the retry is for the
    collapse, not a tax on every commit."""
    model = _bare_model(lambda n, call: ([" one", " two", " three"], [0.0, 0.5, 1.0]))

    tokens, _ = model._transcribe_guarded(np.zeros(16_000 * 2, dtype=np.float32))

    assert tokens == [" one", " two", " three"]
    assert len(model.calls) == 1, "healthy decode must not pay for a retry"


def test_collapsed_decode_is_retried_with_padding_on_both_ends():
    """Zero words out of ten seconds of audio is the collapse signature."""

    def script(n, call):
        if call == 1:
            return [], []
        return ([" recovered", " text"], [0.3, 0.8])

    model = _bare_model(script)
    tokens, timestamps = model._transcribe_guarded(np.zeros(16_000 * 10, dtype=np.float32))

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
        return ([" solitary"], [0.4]) if call == 1 else ([], [])

    model = _bare_model(script)
    tokens, timestamps = model._transcribe_guarded(np.zeros(16_000 * 10, dtype=np.float32))

    assert len(model.calls) == 2, "exactly one retry, never a loop"
    assert tokens == [" solitary"] and timestamps == [0.4]


def test_short_region_is_judged_on_its_own_length():
    """The threshold is words per second, so a 0.5 s region yielding one word
    is plausible and must not be retried."""
    model = _bare_model(lambda n, call: ([" hi"], [0.0]))

    model._transcribe_guarded(np.zeros(8_000, dtype=np.float32))

    assert len(model.calls) == 1


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
    assert adapter._stream_partial_cadence_s == 0.5
    assert adapter._stream_partial_tail_s == 0.0, "the tick decodes the whole window by default"

    off = build_adapter(build_parser().parse_args(base + ["--stream-partial-cadence-s", "0"]))
    assert off._stream_partial_cadence_s == 0.0, "an explicit 0 must survive the default"
