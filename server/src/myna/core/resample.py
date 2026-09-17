"""Sample-rate conversion at the dialect edge.

OpenAI's ``audio/pcm`` is 24 kHz and nothing else; every adapter takes 16 kHz
and rejects anything else rather than resample (``myna.core.capabilities``).
The IE115 transport reconciles the two here, before the adapter sees a chunk,
so the adapters and the session machinery stay dialect-blind.

The method is the textbook one: upsample by ``L`` (zero insertion), low-pass
with a windowed-sinc FIR at the lower Nyquist, decimate by ``M``, computed as
a polyphase gather so the zeros are never materialised. It is stateful on
purpose: a client's 100 ms appends must resample to the same samples the whole
utterance would, so the filter history and the output phase carry across
``feed`` calls, and ``flush`` drains the tail at the utterance boundary.
"""

from __future__ import annotations

from math import gcd

import numpy as np
from numpy.typing import NDArray

from myna.core.audio import PcmFramer

# Half-width of the sinc window in zero crossings at the lower rate. 32 gives a
# transition band of ~1.4 kHz around an 8 kHz cutoff with a Blackman window, and
# ~150 multiply-adds per output sample: nothing, at 16 kHz.
_HALF_WINDOW = 32


class Resampler:
    """Streaming ``src_hz`` -> ``dst_hz`` converter for mono S16LE PCM bytes."""

    def __init__(self, src_hz: int, dst_hz: int) -> None:
        if src_hz <= 0 or dst_hz <= 0:
            raise ValueError(f"sample rates must be positive, got {src_hz} -> {dst_hz}")
        g = gcd(src_hz, dst_hz)
        self._up = dst_hz // g  # L
        self._down = src_hz // g  # M
        self._identity = self._up == self._down
        # FIR at the high rate L*src, cutoff at the lower of the two Nyquists,
        # gain L to make up for the inserted zeros.
        taps = 2 * _HALF_WINDOW * max(self._up, self._down) + 1
        self._delay = (taps - 1) // 2
        cutoff = min(1 / self._up, 1 / self._down) / 2  # cycles per high-rate sample
        n = np.arange(taps) - self._delay
        h = 2 * cutoff * np.sinc(2 * cutoff * n) * np.blackman(taps)
        h *= self._up / h.sum()
        # One zero tap past the end: a gathered index of ``taps`` reads as 0.
        self._h: NDArray[np.float64] = np.concatenate([h, [0.0]])
        self._taps = taps
        self._per_output = -(-taps // self._up)  # input samples per output
        self._framer = PcmFramer(2)
        self._reset()

    def _reset(self) -> None:
        self._buf: NDArray[np.float64] = np.zeros(0)
        self._base = 0  # global index of _buf[0]
        self._next_out = 0  # global index of the next output sample

    def feed(self, pcm: bytes) -> bytes:
        """Convert what can be converted so far; the rest waits for more."""
        whole = self._framer.feed(pcm)
        if self._identity:
            return whole
        samples = np.frombuffer(whole, dtype="<i2").astype(np.float64)
        self._buf = np.concatenate([self._buf, samples])
        return self._produce()

    def flush(self) -> bytes:
        """Drain the tail at the utterance boundary and start afresh: the
        output ends where the input did, to the sample."""
        self._framer.flush()
        if self._identity:
            return b""
        total = self._base + len(self._buf)
        last_out = -(-total * self._up // self._down) - 1
        need = (last_out * self._down + self._delay) // self._up
        pad = need - (total - 1)
        if pad > 0:
            self._buf = np.concatenate([self._buf, np.zeros(pad)])
        out = self._produce()
        self._reset()
        return out

    def _produce(self) -> bytes:
        up, down, delay, taps = self._up, self._down, self._delay, self._taps
        last_in = self._base + len(self._buf) - 1
        # Output k needs inputs up to (k*M + D) // L, so every k with
        # k*M + D < (last_in + 1) * L is computable.
        last_out = -(-((last_in + 1) * up - delay) // down) - 1
        if last_out < self._next_out:
            return b""
        ks = np.arange(self._next_out, last_out + 1)
        # Output k sits at high-rate index k*M + D; its taps cover the inputs
        # n with 0 <= k*M + D - n*L < taps.
        first_in = -(-(ks * down + delay - taps + 1) // up)
        n = first_in[:, None] + np.arange(self._per_output)[None, :]
        j = ks[:, None] * down + delay - n * up
        j = np.where(j < 0, taps, j)  # before the window: the zero tap
        idx = n - self._base
        x = np.where(idx < 0, 0.0, self._buf[np.clip(idx, 0, len(self._buf) - 1)])
        y = (x * self._h[j]).sum(axis=1)
        self._next_out = last_out + 1
        keep = -(-(self._next_out * down + delay - taps + 1) // up)
        if keep > self._base:
            self._buf = self._buf[keep - self._base :]
            self._base = keep
        return np.clip(np.rint(y), -32768, 32767).astype("<i2").tobytes()
