# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "scipy"]
# ///
"""Synthesize the daemon's session cues into ``client/myna-desktop/sounds/``.

    uv run dev/synth_cues.py

Three sound sets, each a start, stop and error cue in ``sounds/<set>/``:

- ``myna`` (Bird): whistled chirps, the bird the product is named after. Start
  is a "tu-wee" up-slur, stop one relaxed down-slur landing on a low "tuk",
  error a pulsed, noisy alarm "churr", twice.
- ``tine`` (Chime): modal synthesis of a struck bar, a Hann mallet pulse
  into a bank of two-pole resonators (tuned-bar 1:4:10 modes plus a faint
  octave), a little contact noise and a small room. Start is a rising A
  major arpeggio, stop a falling third whose last note is damped like a lid
  closing, error two damped tritone dyads a semitone apart.
- ``hum`` (Voice): the pitch contours of spoken backchannels on an additive
  voiced source with breath: "hm?" rising for start, "mm-hm." falling for
  stop, a clipped buzzy "uh-uh" for error.

Every clip is mono 48 kHz, 0.56 s, starts and ends at exact silence (a clip
that stops above zero clicks), and is loudness matched with ITU-R BS.1770
K-weighting rather than by peak: start at -16 LUFS, stop 2 LU quieter, error
1 LU louder, under a soft-knee limit at -1 dBFS. The noise is seeded, so a
rerun reproduces the committed files. Encoded as Ogg Vorbis with ffmpeg.
"""

from __future__ import annotations

import subprocess
import tempfile
import wave
from pathlib import Path
from typing import Any

import numpy as np
import numpy.typing as npt
from scipy.signal import butter, fftconvolve, lfilter, sosfilt

RATE = 48_000
LEN = 0.56
OUT = Path(__file__).resolve().parent.parent / "client" / "myna-desktop" / "sounds"

Signal = npt.NDArray[np.float64]

rng = np.random.default_rng(7)


def hz(name: str) -> float:
    semis = {"C": -9, "D": -7, "E": -5, "F": -4, "G": -2, "A": 0, "B": 2}[name[0]]
    semis += name.count("#") - name[1:].count("b")
    return 440.0 * 2 ** ((semis + 12 * (int(name[-1]) - 4)) / 12)


def blank() -> Signal:
    return np.zeros(int(LEN * RATE))


def place(buf: Signal, x: Signal, at: float, gain: float = 1.0) -> None:
    i = int(at * RATE)
    n = min(len(x), len(buf) - i)
    buf[i : i + n] += gain * x[:n]


def bandpass(lo: float, hi: float) -> Any:
    return butter(2, [lo, hi], "bp", fs=RATE, output="sos")


# ---------------------------------------------------------------- room ----


def room(t60: float = 0.32, wet: float = 0.16, tone: float = 5000) -> Signal:
    """Synthetic small-room impulse response: a few early taps, then a dark
    decaying noise tail."""
    n = int(t60 * RATE)
    t = np.arange(n) / RATE
    tail = rng.standard_normal(n) * 10 ** (-3 * t / t60)
    tail = sosfilt(butter(2, tone, fs=RATE, output="sos"), tail)
    tail[: int(0.012 * RATE)] = 0
    ir: Signal = tail * wet / np.sqrt(np.sum(tail**2))
    for delay, gain in [(0.0047, 0.09), (0.0081, -0.07), (0.0113, 0.05)]:
        ir[int(delay * RATE)] += gain
    ir[0] = 1.0
    return ir


def reverb(x: Signal, **kw: float) -> Signal:
    return fftconvolve(x, room(**kw))[: len(x)]


# ---------------------------------------------------------------- tine ----

# (frequency ratio, amplitude, t60 scale) per mode: a tuned bar's 1:4:10 with a
# faint octave for warmth; the upper modes die fast, as on a real bar.
BAR = [(1.0, 1.0, 1.0), (2.0, 0.06, 0.5), (4.0, 0.20, 0.22), (10.0, 0.045, 0.07)]


def strike(f0: float, t60: float, hardness: float = 1.0) -> Signal:
    """A Hann-pulse mallet into a bank of two-pole resonators. A softer mallet
    stays in contact longer, which is what darkens it."""
    n = int(LEN * RATE)
    width = int(RATE * 0.0011 / hardness)
    exc = np.zeros(n)
    exc[:width] = np.hanning(width) / (width / 2)
    # a whisper of contact noise: the tick that makes it an object
    contact = int(0.003 * RATE)
    tick = rng.standard_normal(contact) * np.hanning(contact)
    tick = sosfilt(butter(2, 3000, "hp", fs=RATE, output="sos"), tick)
    exc[:contact] += 0.02 * hardness * tick
    out = np.zeros(n)
    cents = rng.uniform(-3, 3)  # a hair of tuning drift per note
    for ratio, amp, scale in BAR:
        f = f0 * ratio * 2 ** (cents / 1200)
        if f > RATE * 0.45:
            continue
        r = 10 ** (-3 / (t60 * scale * RATE))
        w = 2 * np.pi * f / RATE
        out += amp * lfilter([np.sin(w)], [1, -2 * r * np.cos(w), r * r], exc)
    return out


def tine() -> dict[str, Signal]:
    start = blank()
    for i, (note, gain) in enumerate([("A5", 0.55), ("C#6", 0.7), ("E6", 1.0)]):
        place(start, strike(hz(note), 0.7, 1.1), 0.004 + i * 0.036, gain)

    stop = blank()
    place(stop, strike(hz("E6"), 0.45, 1.0), 0.004, 0.8)
    place(stop, strike(hz("A5"), 0.20, 0.7), 0.078, 1.0)

    error = blank()
    for at, (lo, hi), gain in [(0.004, ("E5", "A#5"), 1.0), (0.13, ("D#5", "A5"), 0.85)]:
        place(error, strike(hz(lo), 0.16, 1.2), at, gain)
        place(error, strike(hz(hi), 0.16, 1.2), at + 0.004, 0.8 * gain)

    return {name: reverb(x) for name, x in dict(start=start, stop=stop, error=error).items()}


# ----------------------------------------------------------------- hum ----


def voice(f0: Signal, amp: Signal, tilt: float, breath: float = 0.05) -> Signal:
    """An additive voiced source following the f0 and amplitude contours: a
    harmonic rolloff with a soft formant near 1.1 kHz, plus breath noise."""
    phase = 2 * np.pi * np.cumsum(f0) / RATE
    out = np.zeros_like(f0)
    for k in range(1, 9):
        fk = f0 * k
        env = k**-tilt * (1 + 1.2 * np.exp(-(((fk - 1100) / 450) ** 2)))
        out += np.where(fk < 9000, env, 0) * np.sin(k * phase)
    noise = sosfilt(bandpass(1800, 6000), rng.standard_normal(len(f0)))
    return amp * (out + breath * noise)


def contour(points: list[tuple[float, float]], n: int, smooth: float) -> Signal:
    """Piecewise-linear through `points` (seconds, value), Hann-smoothed."""
    xs, ys = zip(*points, strict=True)
    y = np.interp(np.arange(n) / RATE, xs, ys)
    k = int(smooth * RATE)
    window = np.hanning(2 * k + 1)
    return np.convolve(np.pad(y, k, mode="edge"), window / window.sum(), "same")[k:-k]


def hum() -> dict[str, Signal]:
    n = int(LEN * RATE)

    # "hm?" - one syllable rising a fourth, like a question
    f = contour([(0, hz("E5")), (0.07, hz("E5")), (0.24, hz("A5")), (LEN, hz("A5"))], n, 0.04)
    a = contour([(0, 0), (0.025, 0.7), (0.1, 0.8), (0.24, 1.0), (0.36, 0.75), (0.5, 0)], n, 0.02)
    start = voice(f, a, tilt=2.1)

    # "mm-hm." - two legato syllables settling downwards
    f = contour(
        [(0, hz("G#5")), (0.1, hz("G#5")), (0.15, hz("E5")), (0.3, hz("C#5")), (LEN, hz("C#5"))],
        n,
        0.025,
    )
    a = contour(
        [(0, 0), (0.02, 0.8), (0.09, 0.8), (0.125, 0.35), (0.16, 1.0), (0.3, 0.6), (0.44, 0)],
        n,
        0.012,
    )
    stop = voice(f, a, tilt=2.2)

    # "uh-uh" - two clipped syllables, glottal stops between, buzzier and lower
    f = contour(
        [
            (0, hz("C#5")),
            (0.1, hz("B4")),
            (0.17, hz("B4")),
            (0.19, hz("A4")),
            (0.33, hz("G#4")),
            (LEN, hz("G#4")),
        ],
        n,
        0.008,
    )
    a = contour(
        [
            (0, 0),
            (0.012, 1.0),
            (0.085, 0.95),
            (0.1, 0),
            (0.17, 0),
            (0.183, 1.0),
            (0.3, 0.85),
            (0.33, 0),
        ],
        n,
        0.004,
    )
    error = voice(f, a, tilt=1.2, breath=0.02)

    lead = np.zeros(int(0.003 * RATE))  # the contours leave zero at full slope
    cues = dict(start=start, stop=stop, error=error)
    return {name: reverb(np.concatenate([lead, x])[:n], wet=0.12) for name, x in cues.items()}


# ---------------------------------------------------------------- myna ----


def chirp(f_from: float, f_to: float, dur: float, curve: float = 2.0) -> Signal:
    """A whistled syllable: an exponential-ish glide under a sin^2 envelope,
    with the trace of second harmonic a syrinx gives."""
    n = int(dur * RATE)
    u = np.arange(n) / n
    shape = u**curve if f_to > f_from else 1 - (1 - u) ** curve
    phase = 2 * np.pi * np.cumsum(f_from * (f_to / f_from) ** shape) / RATE
    return np.sin(np.pi * u) ** 2 * (np.sin(phase) + 0.18 * np.sin(2 * phase + 0.3))


def churr(f_from: float, f_to: float) -> Signal:
    """An alarm call: a falling tone with band noise, pulsed at 42 Hz."""
    n = int(0.13 * RATE)
    u = np.arange(n) / n
    carrier = np.sin(2 * np.pi * np.cumsum(f_from * (f_to / f_from) ** u) / RATE)
    rough = sosfilt(bandpass(700, 2600), rng.standard_normal(n))
    pulse = 0.5 * (1 + np.sign(np.sin(2 * np.pi * 42 * u * 0.13))) * 0.85 + 0.15
    pulse = np.convolve(pulse, np.hanning(97) / np.hanning(97).sum(), "same")
    return np.sin(np.pi * u) ** 1.2 * pulse * (carrier + 0.18 * rough / np.std(rough))


def myna() -> dict[str, Signal]:
    start = blank()
    place(start, chirp(1400, 1900, 0.045), 0.004, 0.6)
    place(start, chirp(1700, 2700, 0.11, curve=1.4), 0.07, 1.0)

    stop = blank()
    place(stop, chirp(2500, 1500, 0.14, curve=1.6), 0.004, 1.0)
    place(stop, chirp(1350, 1200, 0.04), 0.17, 0.45)

    error = blank()
    place(error, churr(1300, 1000), 0.004, 1.0)
    place(error, churr(1150, 850), 0.17, 0.85)

    cues = dict(start=start, stop=stop, error=error)
    return {name: reverb(x, wet=0.2, t60=0.4, tone=7000) for name, x in cues.items()}


# -------------------------------------------------------------- output ----

TARGET_LUFS = -16.0
OFFSET_LU = {"start": 0.0, "stop": -2.0, "error": 1.0}
CEILING = 10 ** (-1 / 20)


def loudness(x: Signal) -> float:
    """BS.1770 K-weighted loudness of the active part, relative-gated at
    -20 dB over 50 ms blocks: a cue is too short for the standard's 400 ms."""
    y = lfilter(
        [1.53512485958697, -2.69169618940638, 1.19839281085285],
        [1, -1.69065929318241, 0.73248077421585],
        x,
    )
    y = lfilter([1.0, -2.0, 1.0], [1, -1.99004745483398, 0.99007225036621], y)
    block = int(0.05 * RATE)
    power = np.array([np.mean(y[i : i + block] ** 2) for i in range(0, len(y) - block, block // 2)])
    power = power[power > power.max() * 10 ** (-20 / 10)]
    return float(-0.691 + 10 * np.log10(np.mean(power)))


def limit(x: Signal) -> Signal:
    """Soft knee: untouched below 60% of the ceiling, tanh above it."""
    knee = 0.6 * CEILING
    headroom = CEILING - knee
    over = np.abs(x) > knee
    y = x.copy()
    y[over] = np.sign(x[over]) * (knee + headroom * np.tanh((np.abs(x[over]) - knee) / headroom))
    return y


def finish(cue: str, x: Signal) -> Signal:
    fade = int(0.06 * RATE)
    x = x.copy()
    x[-fade:] *= 0.5 * (1 + np.cos(np.pi * np.arange(fade) / fade))
    x[-RATE // 1000 :] = 0
    x *= 10 ** ((TARGET_LUFS + OFFSET_LU[cue] - loudness(x)) / 20)
    return limit(x)


def write(path: Path, x: Signal) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        pcm = Path(tmp) / "cue.wav"
        with wave.open(str(pcm), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(RATE)
            w.writeframes((np.clip(x, -1, 1) * 32767).astype("<i2").tobytes())
        subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-i", pcm, "-c:a", "libvorbis"]
            + ["-q:a", "6", "-map_metadata", "-1", "-fflags", "+bitexact", path],
            check=True,
        )


def main() -> None:
    # Rendered in this order on purpose: the sets share one seeded noise
    # source, so reordering them changes every clip after the first.
    sets = {"tine": tine(), "hum": hum(), "myna": myna()}
    for name, cues in sets.items():
        for cue, x in cues.items():
            write(OUT / name / f"{cue}.oga", finish(cue, x))


if __name__ == "__main__":
    main()
