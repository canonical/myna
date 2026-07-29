"""LocalAgreement commit-rule unit tests (feature 008, T009).

Synthetic hypothesis sequences — no model loads. Covers: agreement prefix,
revision/drift rejection, force-commit over cap. (The 2026-07-28 strategy
triage removed tail-mutation and fixed-head — see strategies.py.)
"""

from __future__ import annotations

from myna.testbed.streaming.strategies import (
    Hypothesis,
    LocalAgreement,
    Word,
)


def hyp(words_spec) -> Hypothesis:
    """words_spec: list of (text, start, end)."""
    return Hypothesis(words=[Word(t, s, e) for t, s, e in words_spec])


def test_local_agreement_commits_agreed_prefix():
    s = LocalAgreement()
    words = [(f"w{i} ", float(i), i + 0.9) for i in range(6)]
    assert s.commit_rule(None, hyp(words), window_end=6.0, force=False) is None
    second = s.commit_rule(hyp(words), hyp(words), window_end=6.0, force=False)
    assert second is not None
    # tail guard: words ending within 0.5 s of the tail (w5 ends 5.9 > 5.5) held back
    texts = [w.text for w in second.commit_words]
    assert "w4 " in texts and "w5 " not in texts
    assert second.commit_end == 4.9


def test_local_agreement_no_commit_on_revision():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    h2 = hyp([("x0 ", 0.0, 0.9)] + [(f"w{i} ", float(i), i + 0.9) for i in range(1, 6)])
    assert s.commit_rule(h1, h2, window_end=6.0, force=False) is None


def test_local_agreement_rejects_drifted_words():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    shifted = [(t, st + 0.5, e + 0.5) for t, st, e in [(f"w{i} ", float(i), i + 0.9) for i in range(6)]]
    h2 = hyp(shifted)
    assert s.commit_rule(h1, h2, window_end=6.5, force=False) is None  # drift 0.5 s > AGREE_DRIFT_S


def test_local_agreement_force_over_cap():
    s = LocalAgreement()
    h = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(10)])
    d = s.commit_rule(None, h, window_end=10.0, force=True)
    assert d is not None
    texts = [w.text for w in d.commit_words]
    assert "w8 " in texts and "w9 " not in texts
    assert d.commit_end == 8.9


def test_local_agreement_empty_hypothesis():
    s = LocalAgreement()
    assert s.commit_rule(None, Hypothesis(), window_end=1.0, force=True) is None
