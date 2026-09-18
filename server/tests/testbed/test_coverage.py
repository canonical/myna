"""Untranscribed-speech detection (myna.testbed.streaming.coverage).

The signal both CPU decoders' collapse guards fire on: loud audio no token
accounts for. A pause must never register, a region with no speech in it must
not be loud relative to itself, and a gap must be judged on the audio inside
it.
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.testbed.streaming.coverage import (
    UNTRANSCRIBED_GAP_S,
    has_speech,
    untranscribed_gap,
)

RATE = 16_000


def _noise(seconds: float, rms: float, seed: int) -> np.ndarray:
    rng = np.random.default_rng(seed)
    samples = rng.standard_normal(int(seconds * RATE)).astype(np.float32)
    return samples * (rms / np.sqrt(np.mean(samples * samples)))


def _tone(seconds: float, rms: float, seed: int = 11) -> np.ndarray:
    """Stationary room tone: loud, and no more modulated than a fan."""
    return _noise(seconds, rms, seed)


def _speech(seconds: float, rms: float = 0.05, floor: float = 0.006, seed: int = 3) -> np.ndarray:
    """Speech-shaped: 0.4 s bursts over a 0.15 s inter-word floor, inside the
    p90/p10 spread measured on the corpus (4.9 worst clip, 5.6 worst region of
    the deliberately gapless stress clip, 12.7 long-form)."""
    rng = np.random.default_rng(seed)
    out: list[np.ndarray] = []
    while sum(len(c) for c in out) < seconds * RATE:
        for span, level in ((0.4, rms), (0.15, floor)):
            block = rng.standard_normal(int(span * RATE)).astype(np.float32)
            out.append(block * (level / np.sqrt(np.mean(block * block))))
    return np.concatenate(out)[: int(seconds * RATE)]


def _quiet(seconds: float) -> np.ndarray:
    return np.zeros(int(seconds * RATE), dtype=np.float32)


def _onsets(*times: float) -> list[tuple[float, float]]:
    return [(t, t) for t in times]


def test_a_pause_however_long_is_not_an_untranscribed_gap():
    region = np.concatenate([_speech(3.0), _quiet(6.0), _speech(3.0)])

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 2.5, 9.5, 10.5, 11.5)) < UNTRANSCRIBED_GAP_S


def test_a_pause_is_judged_on_its_own_audio_not_the_speech_after_it():
    """The audio measured is the gap itself: a short pause followed by a long
    run of speech is still a pause."""
    region = np.concatenate([_speech(1.0), _quiet(2.5), _speech(8.0)])

    speech = _onsets(0.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0)

    assert untranscribed_gap(region, speech) < UNTRANSCRIBED_GAP_S


def test_speech_with_no_token_in_it_is_a_gap():
    region = _speech(12.0)

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 11.0)) == pytest.approx(9.5)


def test_speech_before_the_first_token_is_a_gap():
    region = _speech(12.0)

    assert untranscribed_gap(region, _onsets(6.0, 7.0, 8.0)) == pytest.approx(6.0)


def test_speech_after_the_last_token_is_a_gap():
    region = _speech(12.0)

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 2.5)) == pytest.approx(9.5)


def test_a_span_accounts_for_the_audio_it_covers():
    """A segment-level time stands for every word inside it."""
    region = _speech(12.0)

    assert untranscribed_gap(region, [(0.0, 11.5)]) < UNTRANSCRIBED_GAP_S
    assert untranscribed_gap(region, _onsets(0.0, 11.5)) == pytest.approx(11.5)


def test_a_silent_region_is_not_loud_relative_to_itself():
    """Otherwise every silent region would be measured against its own noise
    and retried for words that are not there."""
    assert untranscribed_gap(_quiet(12.0), []) == 0.0
    assert untranscribed_gap(_tone(12.0, 0.0005), []) == 0.0


def test_a_region_shorter_than_one_frame_has_no_gap():
    assert untranscribed_gap(_speech(0.01), []) == 0.0


@pytest.mark.parametrize("rms", [0.005, 0.008, 0.012, 0.02, 0.05])
def test_token_free_room_tone_is_not_an_untranscribed_gap(rms):
    """A hold-to-talk window before anyone speaks: the decode is right to
    produce nothing, and no nudge can find words that are not there."""
    assert untranscribed_gap(_tone(20.0, rms), []) == 0.0


def test_room_tone_is_not_speech_and_speech_is():
    assert not has_speech(_tone(20.0, 0.02))
    assert has_speech(_speech(20.0))


def test_a_region_shorter_than_one_frame_holds_no_speech():
    assert not has_speech(_speech(0.01))


def test_a_hole_in_speech_is_still_a_gap_at_the_worst_measured_spread():
    """The failure the guard exists for must survive the room-tone rule, even
    on a region as gapless as the stress clip's worst."""
    region = _speech(14.0, floor=0.0095)

    assert has_speech(region)
    assert untranscribed_gap(region, _onsets(0.5, 1.5, 2.5, 11.0)) == pytest.approx(8.5)
