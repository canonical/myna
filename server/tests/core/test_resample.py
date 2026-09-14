"""The IE115 edge resampler: a stock OpenAI client sends 24 kHz PCM, the
adapters take 16 kHz, and the translation happens here, statefully, chunk by
chunk, so the adapter never learns what rate the wire carried."""

from __future__ import annotations

import numpy as np
import pytest

from myna.core.resample import Resampler


def sine(hz: float, rate: int, seconds: float, amplitude: float = 0.5) -> bytes:
    t = np.arange(int(rate * seconds)) / rate
    return (np.sin(2 * np.pi * hz * t) * amplitude * 32767).astype("<i2").tobytes()


def samples(pcm: bytes) -> np.ndarray:
    return np.frombuffer(pcm, dtype="<i2").astype(np.float64) / 32767


def test_downsampling_keeps_the_tone_and_its_level():
    """24 kHz -> 16 kHz of a 440 Hz tone is the 440 Hz tone at 16 kHz: same
    frequency, same amplitude, once the filter has settled."""
    r = Resampler(24_000, 16_000)
    out = samples(r.feed(sine(440, 24_000, 1.0)) + r.flush())
    expected = samples(sine(440, 16_000, 1.0))
    n = min(len(out), len(expected))
    settled = slice(200, n - 200)
    assert np.abs(out[settled] - expected[settled]).max() < 0.01


def test_chunked_feeding_is_bit_identical_to_one_shot():
    """State across chunks (filter history and phase) must not change a single
    sample: a client's 100 ms appends resample to the same PCM as the whole
    utterance would."""
    pcm = sine(1000, 24_000, 0.5)
    whole = Resampler(24_000, 16_000)
    one_shot = whole.feed(pcm) + whole.flush()
    for size in (4800, 1000, 2):  # 100 ms appends, odd sizes, one sample at a time
        chunked = Resampler(24_000, 16_000)
        pieces = [chunked.feed(pcm[i : i + size]) for i in range(0, len(pcm), size)]
        pieces.append(chunked.flush())
        assert b"".join(pieces) == one_shot, size


@pytest.mark.parametrize("n_in", range(2395, 2406))
def test_output_length_matches_the_ratio_after_flush(n_in):
    """Every output sample whose position falls inside the input is produced,
    none beyond it, whatever phase the input length ends on."""
    r = Resampler(24_000, 16_000)
    out = r.feed(b"\x00" * 2 * n_in) + r.flush()
    assert len(out) == 2 * -(-n_in * 2 // 3)


def test_output_is_aligned_with_the_input_in_time():
    """No group delay: an impulse at 25 ms in comes out at 25 ms, symmetric,
    at the gain the polyphase branch owes it (L / M of the input)."""
    r = Resampler(24_000, 16_000)
    x = np.zeros(2400, dtype="<i2")
    x[600] = 30000
    out = np.frombuffer(r.feed(x.tobytes()) + r.flush(), dtype="<i2").astype(np.int64)
    assert out.argmax() == 400
    assert out[400] == round(30000 * 2 / 3)
    assert (out[395:400] == out[405:400:-1]).all()


def test_the_first_input_sample_counts_and_the_past_is_silence():
    """The first sample is not history to be discarded, and what came before
    the stream started is silence, not a copy of the first sample."""
    r = Resampler(24_000, 16_000)
    x = np.zeros(2400, dtype="<i2")
    x[0] = 30000
    out = np.frombuffer(r.feed(x.tobytes()) + r.flush(), dtype="<i2")
    assert out[0] == round(30000 * 2 / 3)
    r = Resampler(24_000, 16_000)
    dc = np.full(2400, 20000, dtype="<i2").tobytes()
    out = np.frombuffer(r.feed(dc) + r.flush(), dtype="<i2")
    # the centre tap carries 2/3 of the branch, the rest half looks at silence
    assert 0.5 * 20000 < out[0] < 0.95 * 20000
    assert out[800] == 20000


def test_history_stays_bounded_on_a_long_utterance():
    """Only the filter's reach is kept between chunks; a long dictation must
    not accumulate the audio it has already converted."""
    r = Resampler(24_000, 16_000)
    chunk = b"\x00" * 4800
    for _ in range(100):  # ten seconds
        r.feed(chunk)
    assert len(r._buf) < 4800  # less than one chunk, not the whole ten seconds


def test_odd_byte_counts_are_carried_not_dropped():
    """Appends are byte strings; a chunk boundary can split a sample."""
    pcm = sine(300, 24_000, 0.2)
    r = Resampler(24_000, 16_000)
    out = r.feed(pcm[:1001]) + r.feed(pcm[1001:]) + r.flush()
    ref = Resampler(24_000, 16_000)
    assert out == ref.feed(pcm) + ref.flush()


def test_flush_resets_for_the_next_utterance():
    r = Resampler(24_000, 16_000)
    first = r.feed(sine(500, 24_000, 0.3)) + r.flush()
    second = r.feed(sine(500, 24_000, 0.3)) + r.flush()
    assert first == second


def test_upsampling_works_too():
    r = Resampler(8_000, 16_000)
    out = samples(r.feed(sine(440, 8_000, 1.0)) + r.flush())
    expected = samples(sine(440, 16_000, 1.0))
    n = min(len(out), len(expected))
    assert np.abs(out[200 : n - 200] - expected[200 : n - 200]).max() < 0.01


def test_content_above_the_new_nyquist_is_removed():
    """A 10 kHz tone has no place in 16 kHz audio: it must come out as near
    silence, not fold back as a 6 kHz alias."""
    r = Resampler(24_000, 16_000)
    out = samples(r.feed(sine(10_000, 24_000, 1.0)) + r.flush())
    residual = np.sqrt(np.mean(out[400:-400] ** 2))
    assert residual < 0.5 / 300  # under -50 dB relative to the tone


def test_passband_is_flat_up_to_speech_bandwidth():
    """5 kHz sits well inside the 8 kHz passband: level within half a dB."""
    r = Resampler(24_000, 16_000)
    out = samples(r.feed(sine(5_000, 24_000, 1.0)) + r.flush())
    level = np.sqrt(np.mean(out[400:-400] ** 2)) / (0.5 / np.sqrt(2))
    assert abs(level - 1) < 0.06


def test_same_rate_is_identity():
    pcm = sine(440, 16_000, 0.1)
    r = Resampler(16_000, 16_000)
    assert r.feed(pcm) + r.flush() == pcm


def test_full_scale_input_clips_instead_of_wrapping():
    """A full-scale square overshoots after the low-pass (Gibbs); the overshoot
    must clip to full scale, never wrap to the opposite sign."""
    square = np.where(np.arange(4800) % 400 < 200, 32767, -32768).astype("<i2").tobytes()
    r = Resampler(24_000, 16_000)
    out = np.frombuffer(r.feed(square) + r.flush(), dtype="<i2").astype(np.int32)
    # 60 Hz square: plateaus of 133.3 output samples, positive first. The
    # overshoot sits right after each edge, so the check reaches to within
    # three samples of it: a wrapped clip shows there as the opposite sign.
    plateau = 16_000 / 120
    for i in range(1, 22):
        interior = out[int(i * plateau) + 3 : int((i + 1) * plateau) - 3]
        sign = 1 if i % 2 == 0 else -1
        assert (np.sign(interior) == sign).all(), f"plateau {i} changed sign"
        assert np.abs(interior).min() > 20_000
    assert (out == 32767).any() and (out == -32768).any()  # both overshoots clipped


@pytest.mark.parametrize("bad", [(0, 16_000), (16_000, 0), (-1, 16_000)])
def test_rejects_a_rate_that_is_not_positive(bad):
    with pytest.raises(ValueError):
        Resampler(*bad)
