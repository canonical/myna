"""Bounded rolling audio window for streaming decode (feature 008).

The loop appends PCM, decodes regions of the window and retires audio once it
has been processed. The window holds at most ``window_cap_seconds`` of audio:
`fill` accepts only what fits, and the loop must process and retire a full
window before it can take more. That bound holds whether or not processing
produced any text.

Positions are integer sample counts since session start:

- ``received``: end of all audio appended so far.
- ``processed_through``: audio before this has had its final processing.
- ``retained_start``: first buffered sample; ``processed_through`` minus the
  overlap, so a word straddling the boundary is decoded again.

The text-commit watermark is the loop's, in word-timestamp seconds.

Audio lives only in this in-memory buffer and is discarded with the session -
never persisted.
"""

from __future__ import annotations

import numpy as np
from numpy.typing import NDArray

RATE = 16_000  # all current adapters serve 16 kHz mono
_SAMPLE_BYTES = 2


def to_samples(seconds: float) -> int:
    return round(seconds * RATE)


class RollingWindow:
    """S16LE mono PCM in, float32 regions out."""

    def __init__(self, window_cap_seconds: float, overlap_seconds: float):
        if window_cap_seconds < 5.0:
            raise ValueError("window_cap_seconds must be >= 5")
        if not 0.0 <= overlap_seconds < window_cap_seconds:
            raise ValueError("overlap_seconds must be in [0, window_cap_seconds)")
        self.cap = to_samples(window_cap_seconds)
        self.overlap = to_samples(overlap_seconds)
        self._buf = bytearray()
        self.received = 0
        self.retained_start = 0
        self.processed_through = 0

    @property
    def retained(self) -> int:
        return self.received - self.retained_start

    @property
    def full(self) -> bool:
        return self.retained >= self.cap

    @property
    def start(self) -> float:
        """Audio time of the first buffered sample."""
        return self.retained_start / RATE

    @property
    def end(self) -> float:
        """Audio time just past the newest buffered sample."""
        return self.received / RATE

    @property
    def window_seconds(self) -> float:
        return self.retained / RATE

    def fill(self, pcm: bytes | memoryview) -> int:
        """Append as much of ``pcm`` as fits under the cap; return the bytes taken."""
        if len(pcm) % _SAMPLE_BYTES:
            raise ValueError("PCM must hold whole 16-bit samples")
        n = min(len(pcm) // _SAMPLE_BYTES, self.cap - self.retained)
        self._buf.extend(pcm[: n * _SAMPLE_BYTES])
        self.received += n
        return n * _SAMPLE_BYTES

    def samples(self, first: int | None = None, end: int | None = None) -> NDArray[np.float32]:
        """Retained samples in [first, end) as float32 (fresh array); the
        bounds default to the whole window and are clamped to it."""
        lo = 0 if first is None else max(first - self.retained_start, 0)
        hi = self.retained if end is None else min(end - self.retained_start, self.retained)
        span = self._buf[lo * _SAMPLE_BYTES : max(lo, hi) * _SAMPLE_BYTES]
        return np.frombuffer(span, dtype=np.int16).astype(np.float32) / 32768.0

    def retire(self, through: int) -> None:
        """Mark audio before ``through`` processed and drop it, keeping the
        overlap. Monotonic, and never past the audio received."""
        self.processed_through = max(self.processed_through, min(through, self.received))
        keep_from = max(self.retained_start, self.processed_through - self.overlap)
        del self._buf[: (keep_from - self.retained_start) * _SAMPLE_BYTES]
        self.retained_start = keep_from
