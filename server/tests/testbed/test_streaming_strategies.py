"""Strategy commit-rule unit tests (feature 008, T009).

Synthetic hypothesis sequences — no model loads. Covers: agreement prefix,
trailing-segment holdback, stuck-partial escape, force-commit over cap,
fixed-head cut points.
"""

from __future__ import annotations

import numpy as np

from myna.testbed.streaming.strategies import (
    FH_ARM_S,
    FH_FORCE_CUT_S,
    STUCK_PASSES,
    Hypothesis,
    LocalAgreement,
    SegmentText,
    TailMutation,
    FixedHead,
    Word,
)


def hyp(words_spec, segments_spec=()) -> Hypothesis:
    """words_spec: list of (text, start, end); segments_spec: (text, start, end)."""
    return Hypothesis(
        words=[Word(t, s, e) for t, s, e in words_spec],
        segments=[SegmentText(t, s, e) for t, s, e in segments_spec],
    )


# ---------------------------------------------------------------------------
# tail-mutation
# ---------------------------------------------------------------------------


def test_tail_mutation_commits_all_but_trailing():
    s = TailMutation()
    h = hyp(
        [("a ", 0, 1), ("b ", 1, 2), ("c ", 2, 3), ("d ", 3, 4)],
        [("a b", 0, 2), ("c d", 2, 4)],
    )
    d = s.commit_rule(None, h, window_end=4.0, force=False)
    assert d.commit_text == "a b"
    assert d.commit_end == 2.0
    assert d.unstable_text == "c d"


def test_tail_mutation_single_segment_all_unstable():
    s = TailMutation()
    h = hyp([("a ", 0, 1)], [("a", 0, 1)])
    d = s.commit_rule(None, h, window_end=1.0, force=False)
    assert d.commit_text == ""
    assert d.unstable_text == "a"


def test_tail_mutation_stuck_partial_escape():
    s = TailMutation()
    # Segment texts carry leading spaces (faster-whisper natural spacing);
    # the merged commit keeps them — the loop sheds the utterance-edge one.
    h = hyp([("a ", 0, 1), ("b ", 1, 2)], [(" a", 0, 1), (" b", 1, 2)])
    decision = None
    for _ in range(STUCK_PASSES + 2):
        decision = s.commit_rule(None, h, window_end=2.0, force=False)
    assert decision is not None
    assert decision.commit_text == " a b"
    assert decision.commit_end == 2.0
    assert decision.unstable_text == ""


def test_tail_mutation_force_over_cap_commits_words():
    s = TailMutation()
    h = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(10)])
    d = s.commit_rule(None, h, window_end=10.0, force=True)
    assert "w8" in d.commit_text and "w9" not in d.commit_text  # tail guard 0.5 s
    assert d.commit_end == 8.9


def test_tail_mutation_skips_high_no_speech_segments():
    s = TailMutation()
    h = Hypothesis(
        words=[Word("a ", 0, 1), Word("b ", 1, 2)],
        segments=[SegmentText("a", 0, 1, no_speech_prob=0.9), SegmentText("b", 1, 2)],
    )
    d = s.commit_rule(None, h, window_end=2.0, force=False)
    assert d.commit_text == ""
    assert d.unstable_text == "b"


# ---------------------------------------------------------------------------
# local-agreement
# ---------------------------------------------------------------------------


def test_local_agreement_commits_agreed_prefix():
    s = LocalAgreement()
    words = [(f"w{i} ", float(i), i + 0.9) for i in range(6)]
    first = s.commit_rule(None, hyp(words), window_end=6.0, force=False)
    assert first.commit_text == ""  # nothing to compare against yet
    second = s.commit_rule(hyp(words), hyp(words), window_end=6.0, force=False)
    # tail guard: words ending within 0.5 s of the tail (w5 ends 5.9 > 5.5) held back
    assert "w4" in second.commit_text and "w5" not in second.commit_text
    assert second.commit_end == 4.9
    assert "w5" in second.unstable_text


def test_local_agreement_no_commit_on_revision():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    h2 = hyp([("x0 ", 0.0, 0.9)] + [(f"w{i} ", float(i), i + 0.9) for i in range(1, 6)])
    d = s.commit_rule(h1, h2, window_end=6.0, force=False)
    assert d.commit_text == ""  # prefix diverged at word 0


def test_local_agreement_rejects_drifted_words():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    shifted = [(t, st + 0.5, e + 0.5) for t, st, e in [(f"w{i} ", float(i), i + 0.9) for i in range(6)]]
    h2 = hyp(shifted)
    d = s.commit_rule(h1, h2, window_end=6.5, force=False)
    assert d.commit_text == ""  # drift 0.5 s > AGREE_DRIFT_S


def test_local_agreement_force_over_cap():
    s = LocalAgreement()
    h = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(10)])
    d = s.commit_rule(None, h, window_end=10.0, force=True)
    assert "w8" in d.commit_text and "w9" not in d.commit_text
    assert d.commit_end == 8.9


# ---------------------------------------------------------------------------
# fixed-head
# ---------------------------------------------------------------------------


def loud_then_silence(loud_s: float, silence_s: float, rate: int = 16_000) -> np.ndarray:
    rng = np.random.default_rng(7)
    loud = (rng.standard_normal(int(loud_s * rate)) * 0.1).astype(np.float32)
    quiet = np.zeros(int(silence_s * rate), dtype=np.float32)
    return np.concatenate([loud, quiet])


def test_fixed_head_no_cut_before_arm():
    s = FixedHead()
    samples = loud_then_silence(5.0, 5.0)  # 10 s < FH_ARM_S
    assert s.observe(samples, 0.0, 10.0) is None


def test_fixed_head_cuts_at_silence_once_armed():
    s = FixedHead()
    samples = loud_then_silence(FH_ARM_S + 1.0, 2.0)
    cut = s.observe(samples, 0.0, FH_ARM_S + 3.0)
    assert cut is not None
    assert cut >= FH_ARM_S


def test_fixed_head_force_cut():
    s = FixedHead()
    samples = (np.random.default_rng(1).standard_normal(int(FH_FORCE_CUT_S * 16_000)) * 0.1).astype(np.float32)
    cut = s.observe(samples, 0.0, FH_FORCE_CUT_S + 0.1)
    assert cut is not None  # force path returns the window end
