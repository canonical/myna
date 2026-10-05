# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "scipy"]
# ///
"""Synthesize the daemon's session cues into ``client/myna-desktop/sounds/``.

    uv run dev/synth_cues.py

Seven sound sets, each a start, stop and error cue in ``sounds/<set>/``:

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
- ``marimba`` (Marimba): Myna's first cues, a marimba-like rising fifth
  (E5-B5) for start, the same falling for stop, a low detuned A3 double note
  for error.
- ``drop`` (Water): water drops. A drop traps a bubble ringing at its
  Minnaert frequency, rising in pitch as it nears the surface. Start is two
  drops, the second higher, stop a drop then a lower one settling, error two
  low wobbling "glugs".
- ``radio`` (Radio): a walkie-talkie, band-limited to 300 Hz - 3.4 kHz with
  a little saturation. Start is a squelch burst and a rising two-tone, stop a
  falling roger beep and the squelch tail of the released key, error two
  carriers 41 Hz apart beating against each other, crackling.
- ``koto`` (Strings): Karplus-Strong plucks. Start is an upward pentatonic
  roll, stop two notes down, the second palm-muted, error two plucks a
  semitone apart buzzing against a sitar bridge.

Every clip is mono 48 kHz, at most 0.56 s, starts and ends at exact silence (a clip
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


def room(
    t60: float = 0.32,
    wet: float = 0.16,
    tone: float = 5000,
    noise: np.random.Generator = rng,
) -> Signal:
    """Synthetic small-room impulse response: a few early taps, then a dark
    decaying noise tail."""
    n = int(t60 * RATE)
    t = np.arange(n) / RATE
    tail = noise.standard_normal(n) * 10 ** (-3 * t / t60)
    tail = sosfilt(butter(2, tone, fs=RATE, output="sos"), tail)
    tail[: int(0.012 * RATE)] = 0
    ir: Signal = tail * wet / np.sqrt(np.sum(tail**2))
    for delay, gain in [(0.0047, 0.09), (0.0081, -0.07), (0.0113, 0.05)]:
        ir[int(delay * RATE)] += gain
    ir[0] = 1.0
    return ir


def reverb(x: Signal, noise: np.random.Generator = rng, **kw: float) -> Signal:
    return fftconvolve(x, room(noise=noise, **kw))[: len(x)]


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
    for at, (lo, hi), gain in [
        (0.004, ("E5", "A#5"), 1.0),
        (0.13, ("D#5", "A5"), 0.85),
    ]:
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
        [
            (0, hz("G#5")),
            (0.1, hz("G#5")),
            (0.15, hz("E5")),
            (0.3, hz("C#5")),
            (LEN, hz("C#5")),
        ],
        n,
        0.025,
    )
    a = contour(
        [
            (0, 0),
            (0.02, 0.8),
            (0.09, 0.8),
            (0.125, 0.35),
            (0.16, 1.0),
            (0.3, 0.6),
            (0.44, 0),
        ],
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


# ------------------------------------------------------------- marimba ----

# The first cues Myna shipped, kept as a set of their own. Each note used to
# stop dead after 0.39 s while still ringing, a click in every cue; it now
# releases to zero over its last 60 ms.
MARIMBA = [(1.0, 1.0, 1.0), (3.93, 0.35, 0.35), (9.2, 0.12, 0.15)]
BELL = [(1.0, 1.0, 1.0), (2.0, 0.4, 0.6), (3.0, 0.15, 0.4), (4.2, 0.08, 0.25)]


def mallet(f0: float, partials: list[tuple[float, float, float]], detune: float) -> Signal:
    t = np.arange(int(0.39 * RATE)) / RATE
    out = np.zeros_like(t)
    for ratio, amp, scale in partials:
        env = amp * np.exp(-t / (0.09 * scale))
        out += env * np.sin(2 * np.pi * f0 * ratio * t)
        if detune:
            out += 0.6 * env * np.sin(2 * np.pi * f0 * ratio * (1 + detune) * t)
    onset = np.minimum(t / 0.008, 1.0)
    release = int(0.06 * RATE)
    out[-release:] *= 0.5 * (1 + np.cos(np.pi * np.arange(release) / release))
    return out * onset * 0.5 * (1 - np.cos(np.pi * onset))


def phrase(
    notes: list[str],
    gap: float,
    partials: list[tuple[float, float, float]],
    detune: float = 0.0,
) -> Signal:
    step = int(gap * RATE)
    parts = [mallet(hz(note), partials, detune) for note in notes]
    out = np.zeros(step * (len(parts) - 1) + len(parts[0]))
    for i, part in enumerate(parts):
        out[i * step : i * step + len(part)] += part
    return out


def marimba() -> dict[str, Signal]:
    return dict(
        start=phrase(["E5", "B5"], 0.085, MARIMBA),
        stop=phrase(["B5", "E5"], 0.085, MARIMBA),
        error=phrase(["A3", "A3"], 0.13, BELL, detune=0.018),
    )


# ---------------------------------------------------------------- drop ----


def drip(f0: float, rise: float = 0.6, decay: float = 0.028) -> Signal:
    """A water drop: the bubble it traps rings at its Minnaert frequency, and
    the pitch climbs by `rise` as the bubble nears the surface."""
    t = np.arange(int(0.3 * RATE)) / RATE
    f = f0 * (1 + rise * (1 - np.exp(-t / 0.04)))
    phase = 2 * np.pi * np.cumsum(f) / RATE
    env = np.exp(-t / decay) * np.minimum(t / 0.0015, 1)
    env *= 0.5 * (1 + np.cos(np.pi * t / t[-1]))
    return env * (np.sin(phase) + 0.12 * np.sin(2 * phase))


def glug(f0: float) -> Signal:
    """A big, slow bubble: falling pitch, a wobble in amplitude."""
    t = np.arange(int(0.16 * RATE)) / RATE
    phase = 2 * np.pi * np.cumsum(f0 * (1 - 0.25 * t / t[-1])) / RATE
    wobble = 1 - 0.45 * (0.5 + 0.5 * np.cos(2 * np.pi * 24 * t))
    env = np.sin(np.pi * t / t[-1]) ** 1.5 * wobble
    return env * (np.sin(phase) + 0.3 * np.sin(2 * phase) + 0.1 * np.sin(3 * phase))


def drop(rooms: np.random.Generator) -> dict[str, Signal]:
    start = blank()
    place(start, drip(1150), 0.004, 0.7)
    place(start, drip(1650, rise=0.7), 0.085, 1.0)

    stop = blank()
    place(stop, drip(1500), 0.004, 0.9)
    place(stop, drip(950, rise=0.35, decay=0.05), 0.11, 1.0)

    error = blank()
    place(error, glug(520), 0.004, 1.0)
    place(error, glug(430), 0.17, 0.9)

    cues = dict(start=start, stop=stop, error=error)
    return {name: reverb(x, noise=rooms, wet=0.24, t60=0.42, tone=6000) for name, x in cues.items()}


# --------------------------------------------------------------- radio ----


def beep(f: float, dur: float) -> Signal:
    """A soft square: three odd harmonics, 4 ms edges."""
    t = np.arange(int(dur * RATE)) / RATE
    x = sum(np.sin(2 * np.pi * f * k * t) / k for k in (1, 3, 5) if f * k < 6000)
    return x * np.minimum(1, np.minimum(t, t[-1] - t) / 0.004)


def squelch(dur: float, fall: float, noise: np.random.Generator) -> Signal:
    t = np.arange(int(dur * RATE)) / RATE
    return noise.standard_normal(len(t)) * np.exp(-t / fall) * np.minimum(t / 0.002, 1)


def crackle(n: int, noise: np.random.Generator) -> Signal:
    x = np.zeros(n)
    for at in noise.integers(0, n - 200, int(60 * n / RATE)):
        x[at : at + 60] += noise.uniform(-1, 1) * np.exp(-np.arange(60) / 8)
    return x


def radio(noise: np.random.Generator, rooms: np.random.Generator) -> dict[str, Signal]:
    """A walkie-talkie, band-limited to 300 Hz - 3.4 kHz: squelch and a rising
    two-tone opens the channel, a falling roger beep and the squelch tail of
    the released key closes it, and two carriers 41 Hz apart beat against each
    other, crackling, for error."""
    start = blank()
    place(start, squelch(0.05, 0.012, noise), 0.004, 0.35)
    place(start, beep(1000, 0.055), 0.05, 0.8)
    place(start, beep(1500, 0.09), 0.115, 0.8)

    stop = blank()
    place(stop, beep(1500, 0.06), 0.004, 0.8)
    place(stop, beep(1000, 0.08), 0.075, 0.8)
    place(stop, squelch(0.12, 0.035, noise), 0.165, 0.45)

    error = blank()
    t = np.arange(int(0.3 * RATE)) / RATE
    f = 950 * (1 - 0.12 * t / t[-1])
    beat = np.sin(2 * np.pi * np.cumsum(f) / RATE) + np.sin(2 * np.pi * np.cumsum(f + 41) / RATE)
    env = np.minimum(1, np.minimum(t / 0.006, (t[-1] - t) / 0.04))
    place(error, env * (0.6 * beat + 0.5 * crackle(len(t), noise)), 0.004)
    place(error, squelch(0.06, 0.02, noise), 0.31, 0.3)

    band = butter(4, [300, 3400], "bp", fs=RATE, output="sos")
    cues = dict(start=start, stop=stop, error=error)
    return {
        name: reverb(
            np.tanh(1.6 * sosfilt(band, x)) / 1.6,
            noise=rooms,
            wet=0.05,
            t60=0.2,
            tone=4000,
        )
        for name, x in cues.items()
    }


# ---------------------------------------------------------------- koto ----


def pluck(
    f0: float,
    noise: np.random.Generator,
    t60: float = 0.6,
    bright: float = 0.5,
    buzz: float = 0.0,
) -> Signal:
    """Karplus-Strong: a noise burst, plucked near the bridge, circulating in
    a fractional delay line with a loss and a lowpass per round trip. `buzz`
    adds the string slapping a sitar bridge (one-sided waveshaping)."""
    n = int(LEN * RATE)
    period = RATE / f0
    d = int(period)
    frac = period - d
    loss = 10 ** (-3 / (t60 * f0))
    burst = noise.uniform(-1, 1, d + 2)
    burst = sosfilt(butter(2, 1200 + 4000 * bright, fs=RATE, output="sos"), burst)
    burst -= np.roll(burst, int(d * 0.18))
    y = np.zeros(n + d + 2)
    y[: d + 2] = burst
    near, far = loss * (0.5 + 0.25 * bright), loss * (0.5 - 0.25 * bright)
    for i in range(d + 2, n + d + 2):
        a = y[i - d] * (1 - frac) + y[i - d - 1] * frac
        b = y[i - d - 1] * (1 - frac) + y[i - d - 2] * frac
        y[i] = near * a + far * b
    out = y[d + 2 :] * np.minimum(np.arange(n) / (0.0008 * RATE), 1)
    if buzz:
        out = out + buzz * np.where(out > 0.15, np.tanh(8 * (out - 0.15)), 0)
    return out / np.max(np.abs(out))


def koto(noise: np.random.Generator, rooms: np.random.Generator) -> dict[str, Signal]:
    start = blank()
    for i, (note, gain) in enumerate([("D5", 0.55), ("E5", 0.6), ("A5", 0.75), ("D6", 1.0)]):
        place(start, pluck(hz(note), noise, t60=0.8, bright=0.6), 0.004 + i * 0.03, gain)

    stop = blank()
    place(stop, pluck(hz("A5"), noise, t60=0.5, bright=0.5), 0.004, 0.85)
    place(stop, pluck(hz("D5"), noise, t60=0.12, bright=0.2), 0.09, 1.0)

    error = blank()
    place(error, pluck(hz("F5"), noise, t60=0.25, bright=0.7, buzz=0.9), 0.004, 1.0)
    place(error, pluck(hz("E5"), noise, t60=0.3, bright=0.7, buzz=0.9), 0.14, 0.9)

    cues = dict(start=start, stop=stop, error=error)
    return {name: reverb(x, noise=rooms, wet=0.14, t60=0.35, tone=5500) for name, x in cues.items()}


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
    sets = {"tine": tine(), "hum": hum(), "myna": myna(), "marimba": marimba()}
    # The later sets have noise sources of their own, so adding them left the
    # first three bit-identical; within them the order matters the same way.
    noise, rooms = np.random.default_rng(11), np.random.default_rng(7)
    sets |= {
        "drop": drop(rooms),
        "radio": radio(noise, rooms),
        "koto": koto(noise, rooms),
    }
    for name, cues in sets.items():
        for cue, x in cues.items():
            write(OUT / name / f"{cue}.oga", finish(cue, x))


if __name__ == "__main__":
    main()
