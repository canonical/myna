"""Audio types shared by the desktop audio pipeline and the testbed.

The audio-push model (see CLAUDE.md) means the *client* owns capture: PCM is
produced client-side (PipeWire on the desktop, scripted/file sources in the
testbed) and pushed to the STT service. The service never touches PipeWire.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Protocol, runtime_checkable


@dataclass(frozen=True)
class AudioFormat:
    """Raw PCM stream description. Default is 16 kHz mono S16LE, the common
    denominator for the candidate models."""

    sample_rate_hz: int = 16_000
    channels: int = 1
    sample_width_bytes: int = 2  # signed little-endian integer samples

    @property
    def bytes_per_second(self) -> int:
        return self.sample_rate_hz * self.channels * self.sample_width_bytes


@dataclass(frozen=True)
class PcmChunk:
    """A contiguous slice of raw PCM audio."""

    data: bytes
    format: AudioFormat

    @property
    def duration_seconds(self) -> float:
        return len(self.data) / self.format.bytes_per_second


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
