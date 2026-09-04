"""Bounded rolling audio window for streaming re-decode (feature 008).

The re-decode loop accumulates PCM while audio arrives, decodes the
*uncommitted* window on a cadence, and advances a committed frontier as
strategies commit text. Frontier advancement drops audio before the frontier
(bounded memory — constitution Principle V); ``window_cap_seconds`` bounds the
uncommitted window, forcing the strategy to commit its oldest stable prefix
beyond that point (I6).

Audio lives only in this in-memory buffer and is discarded with the session —
never persisted.
"""

from __future__ import annotations

import numpy as np

RATE = 16_000  # all current adapters serve 16 kHz mono


class RollingWindow:
    """PCM s16 bytes in, float32 windows out, in audio-time coordinates.

    Time base: seconds since session start. ``frontier`` is the audio time up
    to which text is committed; ``samples_before(frontier)`` are dropped on
    advance. The buffer only ever holds [frontier, now).
    """

    def __init__(self, window_cap_seconds: float = 30.0, overlap_seconds: float = 1.0):
        if window_cap_seconds < 5.0:
            raise ValueError("window_cap_seconds must be >= 5")
        if not 0.0 <= overlap_seconds < window_cap_seconds:
            raise ValueError("overlap_seconds must be in [0, window_cap_seconds)")
        self.window_cap_seconds = window_cap_seconds
        self.overlap_seconds = overlap_seconds
        self._buf = bytearray()
        self.frontier = 0.0  # seconds; audio before this is committed + dropped
        self._received = 0.0  # seconds of audio ever appended (buffer end)

    def append(self, pcm: bytes, duration_seconds: float) -> None:
        self._buf.extend(pcm)
        self._received += duration_seconds

    @property
    def end(self) -> float:
        """Audio time of the newest buffered sample."""
        return self._received

    @property
    def window_seconds(self) -> float:
        """Duration of the uncommitted window [frontier, end)."""
        return self._received - self.frontier

    @property
    def over_cap(self) -> bool:
        return self.window_seconds > self.window_cap_seconds

    def samples(self) -> np.ndarray:
        """The uncommitted window as float32 mono (fresh array)."""
        return np.frombuffer(bytes(self._buf), dtype=np.int16).astype(np.float32) / 32768.0

    def region_before(self, cut_abs: float) -> np.ndarray:
        """Samples in [frontier, cut_abs) as float32 mono (fresh array)."""
        span = max(0, int((cut_abs - self.frontier) * RATE) * 2)
        return np.frombuffer(bytes(self._buf[:span]), dtype=np.int16).astype(np.float32) / 32768.0

    def advance(self, new_frontier: float) -> None:
        """Commit audio time up to ``new_frontier`` and drop it from the buffer.

        Keeps ``overlap_seconds`` of pre-frontier audio when the cut is forced
        (a word may straddle it; the strategy dedupes at merge). Monotonic:
        never moves the frontier backwards.
        """
        if new_frontier <= self.frontier:
            return
        keep_from = new_frontier - self.overlap_seconds
        drop_seconds = max(0.0, keep_from - self.frontier)
        drop_bytes = int(drop_seconds * RATE) * 2
        if drop_bytes > 0:
            del self._buf[:drop_bytes]
        self.frontier = max(self.frontier, new_frontier - self.overlap_seconds)
