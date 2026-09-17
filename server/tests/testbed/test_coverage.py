"""Untranscribed-speech detection (myna.testbed.streaming.coverage).

The signal both CPU decoders' collapse guards fire on: loud audio no token
accounts for. A pause must never register, a silent region must not be loud
relative to itself, and a gap must be judged on the audio inside it.
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.testbed.streaming.coverage import UNTRANSCRIBED_GAP_S, untranscribed_gap

RATE = 16_000


def _loud(seconds: float, rms: float = 0.05, seed: int = 5) -> np.ndarray:
    rng = np.random.default_rng(seed)
    samples = rng.standard_normal(int(seconds * RATE)).astype(np.float32)
    return samples * (rms / np.sqrt(np.mean(samples * samples)))


def _quiet(seconds: float) -> np.ndarray:
    return np.zeros(int(seconds * RATE), dtype=np.float32)


def _onsets(*times: float) -> list[tuple[float, float]]:
    return [(t, t) for t in times]


def test_a_pause_however_long_is_not_an_untranscribed_gap():
    region = np.concatenate([_loud(3.0), _quiet(6.0), _loud(3.0)])

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 2.5, 9.5, 10.5, 11.5)) < UNTRANSCRIBED_GAP_S


def test_a_pause_is_judged_on_its_own_audio_not_the_speech_after_it():
    """The audio measured is the gap itself: a short pause followed by a long
    run of speech is still a pause."""
    region = np.concatenate([_loud(1.0), _quiet(2.5), _loud(8.0)])

    speech = _onsets(0.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0)

    assert untranscribed_gap(region, speech) < UNTRANSCRIBED_GAP_S


def test_loud_audio_with_no_token_in_it_is_a_gap():
    region = _loud(12.0)

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 11.0)) == pytest.approx(9.5)


def test_loud_audio_before_the_first_token_is_a_gap():
    region = _loud(12.0)

    assert untranscribed_gap(region, _onsets(6.0, 7.0, 8.0)) == pytest.approx(6.0)


def test_loud_audio_after_the_last_token_is_a_gap():
    region = _loud(12.0)

    assert untranscribed_gap(region, _onsets(0.5, 1.5, 2.5)) == pytest.approx(9.5)


def test_a_span_accounts_for_the_audio_it_covers():
    """A segment-level time stands for every word inside it."""
    region = _loud(12.0)

    assert untranscribed_gap(region, [(0.0, 11.5)]) < UNTRANSCRIBED_GAP_S
    assert untranscribed_gap(region, _onsets(0.0, 11.5)) == pytest.approx(11.5)


def test_a_silent_region_is_not_loud_relative_to_itself():
    """Otherwise every silent region would be measured against its own noise
    and retried for words that are not there."""
    assert untranscribed_gap(_quiet(12.0), []) == 0.0
    assert untranscribed_gap(_loud(12.0, rms=0.0005), []) == 0.0


def test_a_region_shorter_than_one_frame_has_no_gap():
    assert untranscribed_gap(_loud(0.01), []) == 0.0
