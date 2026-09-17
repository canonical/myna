"""LocalAgreement commit-rule unit tests (feature 008, T009).

Synthetic hypothesis sequences — no model loads. Covers: agreement prefix,
revision/drift rejection, the forced-boundary commit rule. (The 2026-07-28 strategy
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
    assert s.commit_rule(None, hyp(words), window_end=6.0) is None
    second = s.commit_rule(hyp(words), hyp(words), window_end=6.0)
    assert second is not None
    # tail guard: words ending within 0.5 s of the tail (w5 ends 5.9 > 5.5) held back
    texts = [w.text for w in second.commit_words]
    assert "w4 " in texts and "w5 " not in texts
    assert second.commit_end == 4.9


def test_local_agreement_no_commit_on_revision():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    h2 = hyp([("x0 ", 0.0, 0.9)] + [(f"w{i} ", float(i), i + 0.9) for i in range(1, 6)])
    assert s.commit_rule(h1, h2, window_end=6.0) is None


def test_local_agreement_rejects_drifted_words():
    s = LocalAgreement()
    h1 = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    shifted = [
        (t, st + 0.5, e + 0.5) for t, st, e in [(f"w{i} ", float(i), i + 0.9) for i in range(6)]
    ]
    h2 = hyp(shifted)
    assert s.commit_rule(h1, h2, window_end=6.5) is None  # drift 0.5 s > AGREE_DRIFT_S


def test_local_agreement_edges_are_inclusive():
    s = LocalAgreement()
    # Drift of exactly AGREE_DRIFT_S still agrees; a word ending exactly at
    # the tail guard still has enough right context.
    last = hyp([("a ", 0.0, 0.4), ("b ", 1.0, 5.5)])
    current = hyp([("a ", 0.3, 0.7), ("b ", 1.0, 5.5)])
    d = s.commit_rule(last, current, window_end=6.0)
    assert d is not None
    assert [w.text for w in d.commit_words] == ["a ", "b "]
    assert d.commit_end == 5.5


def test_local_agreement_commits_the_prefix_before_a_drifted_word():
    s = LocalAgreement()
    words = [(f"w{i} ", float(i), i + 0.9) for i in range(6)]
    drifted = [
        (t, st + 0.5, e + 0.5) if i == 3 else (t, st, e) for i, (t, st, e) in enumerate(words)
    ]
    d = s.commit_rule(hyp(words), hyp(drifted), window_end=6.0)
    assert d is not None
    assert [w.text for w in d.commit_words] == ["w0 ", "w1 ", "w2 "]


def test_local_agreement_needs_a_previous_pass():
    s = LocalAgreement()
    words = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(6)])
    assert s.commit_rule(None, words, window_end=6.0) is None
    assert s.commit_rule(Hypothesis(), words, window_end=6.0) is None


def test_boundary_commit_holds_back_only_words_the_overlap_will_redecode():
    s = LocalAgreement()
    h = hyp([(f"w{i} ", float(i), i + 0.9) for i in range(10)])
    d = s.boundary_commit(h, cut=10.0, retain_from=9.0)
    assert d is not None
    assert [w.text for w in d.commit_words] == [f"w{i} " for i in range(9)]
    assert d.commit_end == 8.9


def test_boundary_commit_keeps_a_word_whose_start_is_being_retired():
    # Its audio does not survive the cut, so holding it back would lose it.
    s = LocalAgreement()
    d = s.boundary_commit(hyp([("long ", 7.0, 9.8)]), cut=10.0, retain_from=9.0)
    assert d is not None
    assert [w.text for w in d.commit_words] == ["long "]
    assert d.commit_end == 9.8


def test_boundary_commit_edges_are_exact():
    s = LocalAgreement()
    # Ends exactly at the tail guard: enough right context, commits.
    d = s.boundary_commit(hyp([("a ", 9.1, 9.5)]), cut=10.0, retain_from=9.0)
    assert d is not None and d.commit_end == 9.5
    # Starts exactly at the retained start: re-decoded next window, held.
    assert s.boundary_commit(hyp([("b ", 9.0, 9.6)]), cut=10.0, retain_from=9.0) is None


def test_boundary_commit_is_a_prefix():
    s = LocalAgreement()
    h = hyp([("held ", 9.2, 9.8), ("stray ", 8.0, 9.0)])
    assert s.boundary_commit(h, cut=10.0, retain_from=9.0) is None


def test_boundary_commit_on_an_empty_hypothesis():
    assert LocalAgreement().boundary_commit(Hypothesis(), cut=10.0, retain_from=9.0) is None


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


def _continuous_speech(seconds: float, rms: float = 0.05, seed: int = 7) -> np.ndarray:
    """Speech-like audio with no pause in it: syllables of 0.18 s at full
    gain and 0.06 s closures at a quarter, a 60 ms breath every two seconds
    and a phrase loudness redrawn every 3 s. The dynamic range is what makes
    a floor tracked from the signal alone drift up into the speech; flat
    noise (`_speech`) does not reproduce it."""
    rng = np.random.default_rng(seed)
    total = int(seconds * RATE)
    envelope = np.empty(total, dtype=np.float32)
    at = syllable = 0
    while at < total:
        for duration, gain in ((0.18, 1.0), (0.06, 0.25)):
            span = min(int(duration * RATE), total - at)
            envelope[at : at + span] = gain
            at += span
        syllable += 1
        if syllable % 8 == 0 and at < total:
            span = min(int(0.06 * RATE), total - at)
            envelope[at : at + span] = 0.05
            at += span
    phrase = int(3.0 * RATE)
    for start in range(0, total, phrase):
        envelope[start : start + phrase] *= 0.25 + 0.75 * rng.random()
    signal = rng.standard_normal(total).astype(np.float32) * envelope
    return signal * (rms / np.sqrt(np.mean(signal * signal)))


def _drive(cut: SilenceCut, audio: np.ndarray, *, chunk: float = 0.5) -> list:
    """Feed ``audio`` in ``chunk``-second steps as the loop does, retiring to
    each cut with 1 s of overlap, and return the cuts."""
    frontier = 0.0
    taken = []
    for end in np.arange(chunk, len(audio) / RATE + chunk, chunk):
        end = min(float(end), len(audio) / RATE)
        while True:
            skip = cut.unscanned_offset(frontier)
            window = audio[int(frontier * RATE) + skip : int(end * RATE)]
            at = cut.observe(window, frontier, end, offset=skip)
            if at is None:
                break
            taken.append(at)
            frontier = max(0.0, at.at - 1.0)
    return taken


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
    assert 16.4 <= cut_at.at <= 17.5


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
    assert cut_at is not None
    assert (cut_at.at, cut_at.forced) == (60.0, True)


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
            cuts.append(cut_at.at)
            frontier = cut_at.at - 1.0  # RollingWindow keeps 1 s of overlap
    assert len(cuts) == 2, f"expected a cut per pause, got {cuts}"
    # First pause starts at 16 s; the cut lands at the frame where the 0.5 s
    # silence run completes (VAD detection lag included), not at a call
    # boundary. The second re-arms 15 s past the advanced frontier.
    assert 16.4 <= cuts[0] <= 17.5
    assert cuts[1] >= cuts[0] - 1.0 + 15.0
    assert 33.5 <= cuts[1] <= 35.0


def test_silence_cut_restarts_the_silence_run_after_a_cut():
    # Unbroken silence after the first cut: no active frame resets the run,
    # so only the cut itself can make the next pause wait a full
    # SC_SILENCE_CUT_S past the re-armed point.
    cut = SilenceCut()
    audio = np.concatenate([_speech(16.0), _silence(20.0)])
    frontier = 0.0
    cuts = []
    for end in np.arange(0.5, 36.5, 0.5):
        window = audio[int(frontier * RATE) : int(end * RATE)]
        cut_at = cut.observe(window, frontier, float(end))
        if cut_at is not None:
            cuts.append(cut_at.at)
            frontier = cut_at.at - 1.0
    assert len(cuts) == 2, cuts
    rearmed = cuts[0] - 1.0 + 15.0
    assert cuts[1] >= rearmed + 0.5 - 0.03


def test_mark_cut_moves_the_scan_position():
    cut = SilenceCut(arm_seconds=0.0)
    audio = np.concatenate([_speech(1.0), _silence(1.0)])
    cut.mark_cut(2.0)
    # Everything up to 2.0 s counts as already scanned, so the pause is not seen.
    assert cut.observe(audio, 0.0, 2.0) is None


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
    previous = hyp([("w0 ", 0.0, 0.4)])
    assert s.commit_rule(previous, Hypothesis(), window_end=1.0) is None


def test_silence_cut_refuses_samples_that_skip_unscanned_audio():
    cut = SilenceCut()
    audio = _speech(2.0)
    assert cut.observe(audio[:16_000], 0.0, 1.0) is None
    assert cut.unscanned_offset(0.0) == 15_840
    with pytest.raises(ValueError, match="unscanned_offset"):
        cut.observe(audio[16_000:], 0.0, 2.0, offset=16_000)
    assert cut.observe(audio[15_840:], 0.0, 2.0, offset=15_840) is None


def test_speech_between_short_pauses_restarts_the_silence_run():
    cut = SilenceCut()
    parts = [_speech(16.0)]
    for _ in range(12):
        parts += [_silence(0.4), _speech(1.0)]
    audio = np.concatenate(parts)
    for end in np.arange(0.5, len(audio) / RATE, 0.5):
        assert cut.observe(audio[: int(end * RATE)], 0.0, float(end)) is None


def test_the_unscanned_offset_is_the_frame_holding_the_scan_position():
    cut = SilenceCut(force_cut_seconds=600.0)
    n = 60 * RATE + 470
    assert cut.observe(_speech(61.0)[:n], 0.0, n / RATE) is None
    assert cut.unscanned_offset(0.0) == 60 * RATE


def test_a_pause_cut_lands_on_a_frame_boundary():
    cut = SilenceCut(arm_seconds=1.0)
    audio = np.concatenate([_speech(2.0), _silence(2.0)])
    at = cut.observe(audio, 0.0, 4.0)
    assert at is not None
    assert round(at.at * RATE) % 480 == 0


def test_the_first_silence_after_the_arm_point_starts_a_fresh_run():
    cut = SilenceCut(arm_seconds=15.0)
    # Silent before the arm point, and only 0.2 s of it past it.
    audio = np.concatenate([_speech(14.0), _silence(1.2), _speech(1.0)])
    assert cut.observe(audio, 0.0, len(audio) / RATE) is None


# ---------------------------------------------------------------------------
# Noise-floor drift, cut verification and exact framing (audio review, a5-stress)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("seed", [7, 11])
def test_continuous_speech_is_never_cut_as_a_pause(seed):
    """The false-cut regression: 150 s of speech with no pause in it must
    reach the force cut, not be sliced by a drifting noise floor."""
    cuts = _drive(SilenceCut(), _continuous_speech(150.0, seed=seed))

    # 60 s of window, then 60 s more from the 1 s overlap the force cut keeps.
    assert [c.at for c in cuts] == [60.0, 119.0]
    assert all(c.forced for c in cuts)


def test_a_real_pause_in_speech_still_cuts_and_is_verified_silent():
    audio = np.concatenate([_continuous_speech(16.0), _silence(1.0), _continuous_speech(4.0)])

    cuts = _drive(SilenceCut(), audio)

    assert len(cuts) == 1, cuts
    assert 16.4 <= cuts[0].at <= 17.5
    assert not cuts[0].forced and cuts[0].silent


def test_a_pause_holding_a_loud_frame_is_cut_but_not_verified_silent():
    """A burst inside the run (a plosive, a clipped word onset) leaves the
    smoothed signal below the silence threshold, so the run survives - but a
    word may straddle the cut, so it may not retire without overlap."""
    room = _speech(0.4, rms=0.004)  # a pause over a room floor, not digital silence
    quiet = np.concatenate([room, _speech(0.03, rms=0.015), _speech(0.6, rms=0.004)])
    loud = np.concatenate([room, _speech(0.03, rms=0.02), _speech(0.6, rms=0.004)])
    speech = _continuous_speech(16.0)

    verified = _drive(SilenceCut(), np.concatenate([speech, quiet]))
    unverified = _drive(SilenceCut(), np.concatenate([speech, loud]))

    assert [(c.forced, c.silent) for c in verified] == [(False, True)]
    # Same pause, one frame at 0.31x the speech level in it: cut, not verified.
    assert [(c.forced, c.silent) for c in unverified] == [(False, False)]


def test_a_force_cut_is_never_verified_silent():
    cuts = _drive(SilenceCut(force_cut_seconds=20.0), _continuous_speech(25.0))

    assert [(c.at, c.forced, c.silent) for c in cuts] == [(20.0, True, False)]


def test_every_frame_reaches_the_vad_exactly_once():
    """Float frame arithmetic re-fed frames it had already scanned (434
    updates for 299 frames, measured 2026-09-17), which moved cuts around."""
    cut = SilenceCut(force_cut_seconds=600.0)
    seen = []
    inner = cut._vad.update
    cut._vad.update = lambda rms: (seen.append(rms), inner(rms))[1]
    audio = _continuous_speech(9.0)

    _drive(cut, audio, chunk=0.1)

    assert len(seen) == len(audio) // 480


def test_frames_are_not_re_fed_across_a_cut_and_its_overlap():
    cut = SilenceCut(arm_seconds=2.0, force_cut_seconds=600.0)
    seen = []
    inner = cut._vad.update
    cut._vad.update = lambda rms: (seen.append(rms), inner(rms))[1]
    audio = np.concatenate([_continuous_speech(4.0), _silence(1.0), _continuous_speech(4.0)])

    cuts = _drive(cut, audio, chunk=0.1)

    assert len(cuts) == 1, cuts
    # The overlap the loop keeps is re-decoded, never re-scanned: scanning
    # stops at the cut and resumes there.
    assert len(seen) == len(audio) // 480


def test_a_cut_lands_on_an_exact_sample():
    cut = SilenceCut(arm_seconds=1.0)
    audio = np.concatenate([_speech(2.0), _silence(2.0)])

    at = cut.observe(audio, 0.0, 4.0)

    assert at is not None
    assert at.at * RATE == round(at.at * RATE)


def test_the_scan_position_survives_a_window_that_starts_mid_frame():
    """After a forced cut the window starts on an arbitrary sample; frames
    tile forward from there rather than from a moving window origin."""
    cut = SilenceCut(force_cut_seconds=600.0)
    audio = _continuous_speech(4.0)
    assert cut.observe(audio[: 2 * RATE + 37], 0.0, (2 * RATE + 37) / RATE) is None
    cut.mark_cut((2 * RATE + 37) / RATE)

    assert cut.unscanned_offset((2 * RATE + 37) / RATE) == 0
    assert cut.unscanned_offset(1.0) == RATE + 37
