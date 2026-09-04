"""LocalAgreement commit-rule unit tests (feature 008, T009).

Synthetic hypothesis sequences — no model loads. Covers: agreement prefix,
revision/drift rejection, force-commit over cap. (The 2026-07-28 strategy
triage removed tail-mutation and fixed-head — see strategies.py.)
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.testbed.streaming.strategies import (
    Hypothesis,
    LocalAgreement,
    SilenceCut,
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
    shifted = [
        (t, st + 0.5, e + 0.5) for t, st, e in [(f"w{i} ", float(i), i + 0.9) for i in range(6)]
    ]
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


# ---------------------------------------------------------------------------
# SilenceCut (chunked commit, murmure port — 008 US3)
# ---------------------------------------------------------------------------

RATE = 16_000


def _speech(seconds: float, rms: float = 0.05) -> np.ndarray:
    """Deterministic speech-like noise at the given RMS."""
    rng = np.random.default_rng(42)
    samples = rng.standard_normal(int(seconds * RATE)).astype(np.float32)
    return samples * (rms / np.sqrt(np.mean(samples * samples)))


def _silence(seconds: float) -> np.ndarray:
    return np.zeros(int(seconds * RATE), dtype=np.float32)


def test_silence_cut_never_fires_before_arm():
    cut = SilenceCut()
    # 10 s of speech then 2 s of silence, all under the 15 s arm.
    audio = np.concatenate([_speech(10.0), _silence(2.0)])
    assert cut.observe(audio, 0.0, 12.0) is None


def test_silence_cut_fires_on_pause_past_arm():
    cut = SilenceCut()
    # 16 s speech, then silence; observe incrementally (per 0.5 s) like the loop.
    audio = np.concatenate([_speech(16.0), _silence(2.0), _speech(2.0)])
    cut_at = None
    for end in np.arange(0.5, 20.5, 0.5):
        window = audio[: int(end * RATE)]
        cut_at = cut.observe(window, 0.0, float(end))
        if cut_at is not None:
            break
    assert cut_at is not None, "no cut on a 1 s+ pause past the arm"
    # The pause starts at 16 s; the cut lands at the window end once 0.5 s of
    # silence has run (murmure cuts at buffer end, trailing silence included).
    assert 16.4 <= cut_at <= 17.5


def test_silence_cut_ignores_short_pauses():
    cut = SilenceCut()
    # Past the arm, pauses under the 0.5 s cut don't fire.
    audio = np.concatenate(
        [_speech(16.0), _silence(0.3), _speech(2.0), _silence(0.3), _speech(1.0)]
    )
    for end in np.arange(0.5, 19.5, 0.5):
        window = audio[: int(end * RATE)]
        assert cut.observe(window, 0.0, float(end)) is None


def test_silence_cut_force_cut_bounds_window():
    cut = SilenceCut()
    audio = _speech(61.0)  # continuous speech, no pause: the force cut bounds it
    cut_at = None
    for end in np.arange(1.0, 61.5, 1.0):
        window = audio[: int(end * RATE)]
        cut_at = cut.observe(window, 0.0, float(end))
        if cut_at is not None:
            break
    assert cut_at == 60.0


def test_silence_cut_scans_incrementally_after_advance():
    # After a cut the loop advances the frontier (keeping 1 s overlap); the
    # policy must not re-scan the overlap nor lose its noise floor. Drives the
    # policy exactly like the loop: observe per 0.5 s chunk, cut, advance.
    cut = SilenceCut()
    audio = np.concatenate([_speech(16.0), _silence(1.0), _speech(17.0), _silence(1.0)])
    frontier = 0.0
    cuts = []
    for end in np.arange(0.5, 35.5, 0.5):
        window = audio[int(frontier * RATE) : int(end * RATE)]
        cut_at = cut.observe(window, frontier, float(end))
        if cut_at is not None:
            cuts.append(cut_at)
            frontier = cut_at - 1.0  # RollingWindow keeps 1 s of overlap
    assert len(cuts) == 2, f"expected a cut per pause, got {cuts}"
    # First pause starts at 16 s; the cut lands at the frame where the 0.5 s
    # silence run completes (VAD detection lag included), not at a call
    # boundary. The second re-arms 15 s past the advanced frontier.
    assert 16.4 <= cuts[0] <= 17.5
    assert cuts[1] >= cuts[0] - 1.0 + 15.0
    assert 33.5 <= cuts[1] <= 35.0


def test_silence_cut_adapts_to_quiet_speech():
    # Quiet speech (rms ~0.01) above a low noise floor still counts as active
    # (adaptive thresholds, murmure vad.rs parity) — no spurious cut mid-word.
    cut = SilenceCut()
    audio = np.concatenate([_silence(1.0), _speech(18.0, rms=0.01)])
    for end in np.arange(0.5, 19.5, 0.5):
        window = audio[: int(end * RATE)]
        assert cut.observe(window, 0.0, float(end)) is None


def test_local_agreement_empty_hypothesis():
    s = LocalAgreement()
    assert s.commit_rule(None, Hypothesis(), window_end=1.0, force=True) is None
