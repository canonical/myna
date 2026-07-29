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
from myna.testbed.streaming.strategies import Hypothesis, LocalAgreement, Word
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
    I5 (no unstable limbo at end-of-audio).

    I2 is **verbatim** concatenation: committed deltas carry their own
    (natural) whitespace and a consumer reconstructs the transcript by
    concatenating, never by joining with a separator — an injector that
    inserts each delta as it lands produces exactly the final text.
    """
    committed = _committed(events)
    indices = [e.segment_index for e in committed]
    assert indices == list(range(len(committed))), f"non-monotonic indices: {indices}"
    done = events[-1]
    assert isinstance(done, TranscriptionDone), "session must end with done"
    concat = "".join(e.text for e in committed if e.text)
    assert done.text == concat, (
        f"final transcript != verbatim concatenation of committed:\n  done:   {done.text!r}\n  concat: {concat!r}"
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
    prefix (LocalAgreement's ideal input). Word texts carry a **leading**
    space, matching faster-whisper's natural spacing (" word"), and are
    labeled by **absolute** audio position — a real decoder recognizes the
    same audio as the same words after the window frontier advances (labeling
    by window-relative index would relabel old audio as new words)."""

    def decode(samples: np.ndarray, offset: float) -> Hypothesis:
        n = int(len(samples) / 16_000)
        words = [
            Word(
                text=f" {word_per_second}{round(offset + i)}",
                start=offset + i,
                end=offset + i + 0.9,
            )
            for i in range(n)
        ]
        return Hypothesis(words=words)

    return decode


async def fake_audio(seconds: float, chunk_s: float = 0.5):
    n_chunks = int(seconds / chunk_s)
    for _ in range(n_chunks):
        yield PcmChunk(data=b"\x00\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run_loop(seconds, cadence=1.0, cap=30.0):
    events = []

    async def emit(e):
        events.append(e)

    transcript = await run_streaming_loop(
        fake_audio(seconds),
        emit,
        scripted_decode(),
        LocalAgreement(),
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
    events, transcript = await run_loop(seconds=10)
    committed = _committed(events)
    assert len(committed) >= 2, "expected progressive commits mid-session"
    assert _unstable(events), "expected unstable hypotheses during the session"
    assert_append_only_and_complete(events)
    assert_unstable_wellformed(events)
    assert_commit_clears_unstable(events)


@pytest.mark.asyncio
async def test_redecode_loop_bounded_window_completes():
    """I6: a 5 s cap over 12 s of audio keeps the window bounded; commits
    land and nothing is lost (the force path itself is unit-tested on the
    strategy)."""
    events, transcript = await run_loop(seconds=12, cap=5.0)
    assert len(_committed(events)) >= 1
    assert_append_only_and_complete(events)


@pytest.mark.asyncio
async def test_committed_chunks_keep_natural_spacing():
    """I2 mechanics: committed chunks after the first keep their leading
    space (whisper natural spacing), so an injector concatenating them as
    they land reproduces single-spaced text — no missing or doubled spaces."""
    events, transcript = await run_loop(seconds=10)
    committed = _committed(events)
    assert len(committed) >= 2
    assert not committed[0].text.startswith(" "), "first chunk sheds its leading space"
    for e in committed[1:]:
        assert e.text.startswith(" "), f"later chunk lost its leading space: {e.text!r}"
    concat = "".join(e.text for e in committed)
    assert "  " not in concat, f"doubled spaces in concatenation: {concat!r}"
    assert transcript == concat


@pytest.mark.asyncio
async def test_unstable_never_restates_committed_text():
    """Display semantics: an unstable emission is the *uncommitted remainder*
    of the hypothesis — words a previous commit already emitted must not
    reappear (in-field preedit would otherwise duplicate committed text),
    and it keeps its natural leading space once text has been committed."""
    events, _ = await run_loop(seconds=10)
    seen_committed: list[str] = []
    for e in _finals(events):
        if e.disposition == Disposition.COMMITTED:
            seen_committed.append(e.text.strip())
        else:
            for done_text in seen_committed:
                assert done_text not in e.text, (
                    f"unstable restated committed text {done_text!r}: {e.text!r}"
                )
            if seen_committed and e.text:
                assert e.text.startswith(" "), (
                    f"unstable tail after commits must keep its leading space: {e.text!r}"
                )


@pytest.mark.asyncio
async def test_batch_degenerate_event_stream():
    """I7 harness sanity: a canonical batch event list passes."""
    events = [
        TranscriptionFinal(text="hello world", disposition=Disposition.COMMITTED, segment_index=None),
        TranscriptionDone(text="hello world"),
    ]
    assert_batch_degenerate(events)


# ---------------------------------------------------------------------------
# Overlap-dedupe units (I2 mechanics) — the commit-boundary duplication bugs
# observed live 2026-07-27/28 ("or or", "I bus. bus")
# ---------------------------------------------------------------------------

from myna.testbed.streaming.loop import _alignment_drop, _drop_committed


def test_drop_committed_survives_silence_compression():
    # Live bug (2026-07-28): "...quite well." committed, then ~16 s of
    # silence, then "My name is Charlie" — the VAD-free re-decode compressed
    # the pause and re-timed the already-committed overlap words ~16 s late,
    # so a timestamp-gated dedupe kept them: "quite well. well. My name is
    # Charlie". Alignment must drop them regardless of timestamps.
    committed = ["pre-edit", "is", "actually", "working", "quite", "well"]
    through = 50.0
    words = [
        Word(text=" well.", start=66.0, end=66.5),   # re-timed old word
        Word(text=" My", start=66.6, end=66.9),
        Word(text=" name", start=66.9, end=67.2),
        Word(text=" is", start=67.2, end=67.4),
        Word(text=" Charlie.", start=67.4, end=68.0),
    ]
    kept = _drop_committed(words, committed, through)
    assert [w.text for w in kept] == [" My", " name", " is", " Charlie."]


def test_drop_committed_merged_boundary_token():
    # Live bug (2026-07-28, Spanish): " Mi nombre es Carlos." committed, a
    # pause, then the re-decode merged the overlap words into ONE token
    # (" escarlos.") — word-text alignment can't match a merged token, so it
    # was committed again. Character-level (squashed) alignment must drop it
    # even though its timestamp is late (silence compression).
    committed = ["mi", "nombre", "es", "carlos"]
    through = 20.0
    words = [Word(text=" escarlos.", start=21.0, end=21.6)]
    assert _drop_committed(words, committed, through) == []
    # And the squashed equivalent with a genuinely-new tail keeps the tail:
    words = [
        Word(text=" escarlos.", start=21.0, end=21.6),
        Word(text=" García", start=21.6, end=22.1),
    ]
    kept = _drop_committed(words, committed, through)
    assert [w.text for w in kept] == [" García"]


def test_drop_committed_keeps_genuine_repetition():
    # "no. No way.": the overlap re-decode re-transcribes the OLD "no" too —
    # the leftmost match drops only that instance; the genuine one survives.
    committed = ["i", "said", "no"]
    through = 10.0
    words = [
        Word(text=" no", start=9.2, end=9.9),    # old instance (in overlap)
        Word(text=" No", start=10.5, end=10.9),  # genuinely new
        Word(text=" way.", start=10.9, end=11.4),
    ]
    kept = _drop_committed(words, committed, through)
    assert [w.text for w in kept] == [" No", " way."]


def test_alignment_drop_anchors_on_frontier_despite_overlap_churn():
    # The re-decode changed earlier overlap words ("in some way or" → "and
    # some white or"); the old exact prefix==suffix match returned 0 and the
    # boundary word leaked through ("or or"). The frontier-anchored suffix
    # match must find "or" and drop through it.
    tail = ["in", "some", "way", "or"]
    new = ["and", "some", "white", "or", "other"]
    assert _alignment_drop(tail, new) == 4  # and/some/white/or all drop


def test_alignment_drop_not_partitioned_by_later_matches():
    # Genuinely-new words AFTER the boundary ("that he has") match older
    # committed words; a greedy global matcher partitions the search space
    # and loses the frontier run. Direct suffix search must still find it.
    tail = ["he", "began", "to", "wish", "that", "he", "had", "in", "some", "way", "or"]
    new = ["and", "some", "white", "or", "other", "that", "he", "has", "sent"]
    assert _alignment_drop(tail, new) == 4


def test_alignment_drop_full_frontier_run():
    tail = ["he", "began", "to"]
    new = ["he", "began", "to", "wish", "that"]
    assert _alignment_drop(tail, new) == 3


def test_alignment_drop_leftmost_occurrence_for_repeats():
    # A genuinely repeated phrase keeps its second occurrence.
    tail = ["way", "or"]
    new = ["way", "or", "way", "or", "other"]
    assert _alignment_drop(tail, new) == 2


def test_alignment_drop_no_frontier_match():
    assert _alignment_drop(["a", "b"], ["x", "y", "z"]) == 0
    assert _alignment_drop([], ["x"]) == 0
    assert _alignment_drop(["a"], []) == 0


def test_alignment_drop_abstains_when_claimed_region_exceeds_overlap():
    # Watermark regression (2026-07-28, stream-2277-02 tail-mutation):
    # committed "...Then he rang the ball. No answer."; the tail decode of
    # genuinely-new audio said "he rang again this time harder still no
    # answer" and the clipped overlap never re-surfaced the OLD "no
    # answer". The only suffix occurrence sat at the END of the new stream
    # and the unbounded search dropped all 9 genuinely-new words. The old
    # region re-transcribes only the window's 1 s overlap — it can never be
    # 9 words — so the alignment must abstain entirely.
    tail = ["perhaps", "could", "do", "it", "up", "here", "then", "he", "rang", "the", "ball", "no", "answer"]
    new = ["he", "rang", "again", "this", "time", "harder", "still", "no", "answer"]
    assert _alignment_drop(tail, new) == 0


def test_alignment_drop_abstains_instead_of_falling_through_to_short_suffix():
    # A 6-word claim already exceeds the overlap bound — and the 2-char
    # suffix "er" lives inside "harder", so falling through to shorter
    # suffixes would eat the same genuinely-new words more loosely.
    # Abstention must be total (no shorter-suffix retry, no partial drop).
    tail = ["this", "time", "harder"]
    new = ["he", "rang", "again", "this", "time", "harder", "still", "no", "answer"]
    assert _alignment_drop(tail, new) == 0
    # The same frontier run within the physical overlap bound still drops:
    new_short = ["this", "time", "harder", "still", "no", "answer"]
    assert _alignment_drop(tail, new_short) == 3


def test_drop_committed_keeps_new_tail_ending_in_frontier_repeat():
    # Full `_drop_committed` path for the watermark regression: nothing may
    # be dropped even though the new tail ends with the committed frontier
    # phrase.
    committed = ["then", "he", "rang", "the", "ball", "no", "answer"]
    through = 24.0
    words = [
        Word(text=" He", start=24.2, end=24.5),
        Word(text=" rang", start=24.5, end=24.8),
        Word(text=" again", start=24.8, end=25.2),
        Word(text=" this", start=25.2, end=25.5),
        Word(text=" time", start=25.5, end=25.8),
        Word(text=" harder.", start=25.8, end=26.3),
        Word(text=" Still", start=26.3, end=26.6),
        Word(text=" no", start=26.6, end=26.8),
        Word(text=" answer.", start=26.8, end=27.2),
    ]
    kept = _drop_committed(words, committed, through)
    assert [w.text for w in kept] == [w.text for w in words]


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
