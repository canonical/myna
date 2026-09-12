"""Audio primitives every corpus builder shares.

Both tiers write the same house format (16 kHz mono S16LE) and the same seeded
noise, so these live in one module and are pinned once. The seed matters more
than it looks: a corpus that does not regenerate byte for byte gets a new
``corpus_id``, and every number previously scored against it stops being
comparable.
"""

from __future__ import annotations

import array
import wave

import pytest

from myna.benchmarker._audio import (
    NOISE_SEED,
    NOISE_SNR_DB,
    RATE,
    mix_noise,
    read_wav_meta,
)
from myna.benchmarker._audio import write_wav as write_wav_pcm


def tone(seconds: float = 0.25, rate: int = RATE) -> array.array:
    return array.array("h", [(i % 400) - 200 for i in range(int(seconds * rate))])


def write_wav(path, seconds: float = 0.25, rate: int = RATE, channels: int = 1) -> None:
    with wave.open(str(path), "w") as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(2)
        wf.setframerate(rate)
        wf.writeframes(tone(seconds, rate).tobytes() * channels)


# ─── write_wav / read_wav_meta ──────────────────────────────────────────────────────────


def test_write_wav_round_trips_through_read_wav_meta(tmp_path):
    path = tmp_path / "clip.wav"
    duration = write_wav_pcm(path, tone(0.5), RATE)
    assert duration == pytest.approx(0.5)
    assert read_wav_meta(path) == (0.5, RATE, 1)


def test_write_wav_produces_16_khz_mono_s16le(tmp_path):
    path = tmp_path / "clip.wav"
    write_wav_pcm(path, tone(), RATE)
    with wave.open(str(path), "r") as wf:
        assert (wf.getnchannels(), wf.getsampwidth(), wf.getframerate()) == (1, 2, RATE)


def test_read_wav_meta_reports_stereo_and_odd_rates(tmp_path):
    path = tmp_path / "stereo.wav"
    write_wav(path, seconds=0.5, rate=8_000, channels=2)
    duration, rate, channels = read_wav_meta(path)
    assert (rate, channels) == (8_000, 2)
    assert duration == pytest.approx(0.5)


def test_read_wav_meta_of_an_empty_clip_is_zero_seconds(tmp_path):
    path = tmp_path / "empty.wav"
    write_wav_pcm(path, array.array("h"), RATE)
    assert read_wav_meta(path)[0] == 0.0


# ─── mix_noise ───────────────────────────────────────────────────────────────


def test_mix_noise_is_deterministic_for_a_given_seed():
    clean = tone(0.1)
    assert mix_noise(clean, NOISE_SNR_DB, NOISE_SEED) == mix_noise(clean, NOISE_SNR_DB, NOISE_SEED)


def test_mix_noise_changes_the_signal_and_keeps_its_length():
    clean = tone(0.1)
    noisy = mix_noise(clean, NOISE_SNR_DB, NOISE_SEED)
    assert len(noisy) == len(clean)
    assert noisy != clean


def test_mix_noise_stays_inside_the_s16_range():
    loud = array.array("h", [32000] * 1000)
    assert all(-32768 <= s <= 32767 for s in mix_noise(loud, 0.0, NOISE_SEED))
