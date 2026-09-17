"""Audio types shared by the desktop audio pipeline and the testbed.

The audio-push model (see CLAUDE.md) means the *client* owns capture: PCM is
produced client-side (PipeWire on the desktop, scripted/file sources in the
testbed) and pushed to the STT service. The service never touches PipeWire.
"""

from __future__ import annotations

import logging
from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Protocol, runtime_checkable

_log = logging.getLogger(__name__)


@dataclass(frozen=True)
class AudioFormat:
    """Raw PCM stream description. Default is 16 kHz mono S16LE, the common
    denominator for the candidate models."""

    sample_rate_hz: int = 16_000
    channels: int = 1
    sample_width_bytes: int = 2  # signed little-endian integer samples

    @property
    def frame_bytes(self) -> int:
        """Bytes of one sample frame: a sample for every channel."""
        return self.channels * self.sample_width_bytes

    @property
    def bytes_per_second(self) -> int:
        return self.sample_rate_hz * self.frame_bytes


@dataclass(frozen=True)
class PcmChunk:
    """A contiguous slice of raw PCM audio."""

    data: bytes
    format: AudioFormat

    @property
    def duration_seconds(self) -> float:
        return len(self.data) / self.format.bytes_per_second


class PcmFramer:
    """Cuts a byte stream into whole sample frames. A frame split across
    appends is carried to the next one; at the utterance boundary a partial
    frame cannot be completed, so ``flush`` discards it, loudly, rather than
    pad it or let it straddle into the next utterance."""

    def __init__(self, frame_bytes: int) -> None:
        if frame_bytes < 1:
            raise ValueError(f"a sample frame is at least one byte, got {frame_bytes}")
        self._frame_bytes = frame_bytes
        self._pending = b""

    def feed(self, data: bytes) -> bytes:
        if self._pending:
            data = self._pending + data
        whole = len(data) - len(data) % self._frame_bytes
        self._pending = data[whole:]
        return data[:whole]

    def flush(self) -> None:
        if self._pending:
            _log.warning(
                "discarding %d byte(s) of a partial PCM sample frame at the utterance boundary",
                len(self._pending),
            )
            self._pending = b""


@runtime_checkable
class AudioSource(Protocol):
    """Produces PCM chunks. Implementations control pacing: a real-time source
    sleeps between chunks to mimic live capture; a batch source does not.

    Audio is never persisted — sources stream from memory, a capture stack,
    or read-only test fixtures.
    """

    @property
    def format(self) -> AudioFormat: ...

    def chunks(self) -> AsyncIterator[PcmChunk]: ...
