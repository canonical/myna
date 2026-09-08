"""Audio primitives shared by every corpus builder.

One definition of the house WAV format (16 kHz mono S16LE) and of the seeded
noise mixer, so the synthetic fixture tier and the recorded LibriSpeech tier
cannot drift on either. ``NOISE_SEED`` is fixed on purpose: regenerating a
corpus must reproduce it byte for byte, or its ``corpus_id`` changes and every
number scored against it stops being comparable.
"""

from __future__ import annotations

import math
import random
import wave
from array import array
from pathlib import Path

RATE = 16_000
NOISE_SNR_DB = 10.0
NOISE_SEED = 20260612  # fixed: regeneration must be deterministic


def mix_noise(samples: array, snr_db: float = NOISE_SNR_DB, seed: int = NOISE_SEED) -> array:
    """Add seeded Gaussian noise at the given signal-to-noise ratio."""
    rms = math.sqrt(sum(s * s for s in samples) / len(samples))
    sigma = rms / (10.0 ** (snr_db / 20.0))
    rng = random.Random(seed)
    noisy = array("h", bytes(2 * len(samples)))
    for i, s in enumerate(samples):
        noisy[i] = max(-32768, min(32767, int(s + rng.gauss(0.0, sigma))))
    return noisy


def write_wav(path: Path, samples: array, rate: int = RATE) -> float:
    """Write S16LE mono WAV; returns duration in seconds."""
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(samples.tobytes())
    return len(samples) / rate


def read_wav_meta(path: Path) -> tuple[float, int, int]:
    """(duration_seconds, sample_rate, channels) for an existing WAV."""
    with wave.open(str(path), "rb") as wav:
        frames = wav.getnframes()
        rate = wav.getframerate()
        return frames / rate, rate, wav.getnchannels()
