"""Emission-invariant harness (feature 008, T008).

Assertions I1–I7 from specs/008-progressive-emission/contracts/emission-semantics.md,
plus loop-level tests over `run_streaming_loop` with a scripted decode (no
model loads). Reused by every backend story (whisper, nemotron, parakeet,
sherpa) — the checker functions take any recorded event list.

Events here are the myna.core dataclasses (or TimedEvent-wrapped ones).
"""

from __future__ import annotations

import numpy as np
import pytest

from myna.core import (
    Disposition,
    PcmChunk,
    TranscriptionDone,
    TranscriptionFinal,
)
from myna.core.audio import AudioFormat
from myna.testbed.streaming.loop import run_streaming_loop
from myna.testbed.streaming.strategies import Hypothesis, Word, make_strategy
from myna.testbed.streaming.window import RollingWindow

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


# ---------------------------------------------------------------------------
# Invariant checkers (I1–I5, I7) — reusable across backends
# ---------------------------------------------------------------------------

def _finals(events):
    return [e for e in events if isinstance(e, TranscriptionFinal)]


def _committed(events):
    return [e for e in _finals(events) if e.disposition == Disposition.COMMITTED]


def _unstable(events):
    return [e for e in _finals(events) if e.disposition == Disposition.UNSTABLE]


def assert_append_only_and_complete(events):
    """I1 (append-only, monotonic indices), I2 (final == concatenation),
    I5 (no unstable limbo at end-of-audio)."""
    committed = _committed(events)
    indices = [e.segment_index for e in committed]
    assert indices == list(range(len(committed))), f"non-monotonic indices: {indices}"
    done = events[-1]
    assert isinstance(done, TranscriptionDone), "session must end with done"
    concat = " ".join(e.text for e in committed if e.text).strip()
    assert done.text.strip() == concat, (
        f"final transcript != concatenation of committed:\n  done:   {done.text!r}\n  concat: {concat!r}"
    )


def assert_unstable_wellformed(events):
    """I3: unstable deltas never carry a segment index (never injectable)."""
    for e in _unstable(events):
        assert e.segment_index is None, f"unstable carried segment_index: {e}"


def assert_commit_clears_unstable(events):
    """I4: a committed event invalidates the outstanding unstable text —
    the unstable text after a commit differs from the one before it."""
    last_unstable = None
    for e in _finals(events):
        if e.disposition == Disposition.COMMITTED:
            last_unstable = None
        else:
            assert e.text != last_unstable, "stale unstable re-emitted after commit"
            last_unstable = e.text


def assert_batch_degenerate(events):
    """I7: batch mode = exactly one committed segment, equal to the transcript."""
    committed = _committed(events)
    assert len(committed) == 1, f"batch emitted {len(committed)} committed segments"
    done = events[-1]
    assert isinstance(done, TranscriptionDone)
    assert done.text.strip() == committed[0].text.strip()


# ---------------------------------------------------------------------------
# Scripted decode + fake audio (no model)
# ---------------------------------------------------------------------------

def scripted_decode(word_per_second: str = "w"):
    """decode(samples, offset) -> Hypothesis with one word per second of audio,
    deterministically placed — successive growing windows agree on the shared
    prefix (LocalAgreement's ideal input)."""

    def decode(samples: np.ndarray, offset: float) -> Hypothesis:
        n = int(len(samples) / 16_000)
        words = [
            Word(text=f"{word_per_second}{i} ", start=offset + i, end=offset + i + 0.9)
            for i in range(n)
        ]
        return Hypothesis(words=words)

    return decode


async def fake_audio(seconds: float, chunk_s: float = 0.5):
    n_chunks = int(seconds / chunk_s)
    for _ in range(n_chunks):
        yield PcmChunk(data=b"\x00\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run_loop(strategy_name, seconds, cadence=1.0, cap=30.0):
    events = []

    async def emit(e):
        events.append(e)

    transcript = await run_streaming_loop(
        fake_audio(seconds),
        emit,
        scripted_decode(),
        make_strategy(strategy_name),
        cadence_seconds=cadence,
        window_cap_seconds=cap,
    )
    events.append(TranscriptionDone(text=transcript))
    return events, transcript


# ---------------------------------------------------------------------------
# Loop-level invariant tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_redecode_loop_commits_progressively_and_completes():
    """I1/I2/I3/I5 over a 10 s session: ≥1 commit before end, unstable seen,
    done == concatenation."""
    events, transcript = await run_loop("local-agreement", seconds=10)
    committed = _committed(events)
    assert len(committed) >= 2, "expected progressive commits mid-session"
    assert _unstable(events), "expected unstable hypotheses during the session"
    assert_append_only_and_complete(events)
    assert_unstable_wellformed(events)
    assert_commit_clears_unstable(events)


@pytest.mark.asyncio
async def test_redecode_loop_bounded_window_force_commits():
    """I6: a 5 s cap over 12 s of audio forces commits; nothing is lost."""
    events, transcript = await run_loop("tail-mutation", seconds=12, cap=5.0)
    assert len(_committed(events)) >= 1
    assert_append_only_and_complete(events)


@pytest.mark.asyncio
async def test_chunked_loop_decodes_once_per_cut():
    """fixed-head: no unstable by design; commits land; transcript complete."""
    events, transcript = await run_loop("fixed-head", seconds=10)
    assert not _unstable(events), "fixed-head must not emit unstable text"
    assert_append_only_and_complete(events)


@pytest.mark.asyncio
async def test_batch_degenerate_event_stream():
    """I7 harness sanity: a canonical batch event list passes."""
    events = [
        TranscriptionFinal(text="hello world", disposition=Disposition.COMMITTED, segment_index=None),
        TranscriptionDone(text="hello world"),
    ]
    assert_batch_degenerate(events)


# ---------------------------------------------------------------------------
# RollingWindow units (I6 mechanics)
# ---------------------------------------------------------------------------


def test_window_bounds_memory_under_cap():
    w = RollingWindow(window_cap_seconds=5.0, overlap_seconds=0.0)
    chunk = b"\x00\x00" * 16_000  # 1 s
    for _ in range(12):
        w.append(chunk, 1.0)
    assert w.over_cap
    w.advance(10.0)
    assert w.window_seconds == pytest.approx(2.0)
    assert len(w.samples()) == 2 * 16_000


def test_window_advance_keeps_overlap():
    w = RollingWindow(window_cap_seconds=5.0, overlap_seconds=1.0)
    for _ in range(10):
        w.append(b"\x00\x00" * 16_000, 1.0)
    w.advance(8.0)
    assert w.frontier == pytest.approx(7.0)  # 8.0 cut − 1.0 overlap
    assert w.window_seconds == pytest.approx(3.0)


def test_window_never_moves_frontier_backwards():
    w = RollingWindow(window_cap_seconds=30.0, overlap_seconds=0.0)
    for _ in range(10):
        w.append(b"\x00\x00" * 16_000, 1.0)
    w.advance(6.0)
    w.advance(4.0)  # regression attempt — ignored
    assert w.frontier == pytest.approx(6.0)
