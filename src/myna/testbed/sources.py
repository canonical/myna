"""Audio sources for the harness.

Phase 0 needs only synthetic sources; Phase 1 adds a virtual-PipeWire source
playing real recordings at real-time rate, and a WAV-file source for batch
runs. All sources implement ``myna.core.AudioSource``.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator

from myna.core import AudioFormat, PcmChunk


class SilenceSource:
    """Silent PCM of a fixed duration.

    ``realtime=True`` paces chunks at capture rate (sleeping one chunk
    duration per chunk), mimicking live push-to-talk audio; ``False`` streams
    as fast as the consumer accepts, for contract tests.
    """

    def __init__(
        self,
        duration_seconds: float,
        *,
        format: AudioFormat | None = None,
        chunk_seconds: float = 0.1,
        realtime: bool = False,
    ) -> None:
        self._format = format or AudioFormat()
        self._duration = duration_seconds
        self._chunk_seconds = chunk_seconds
        self._realtime = realtime

    @property
    def format(self) -> AudioFormat:
        return self._format

    async def chunks(self) -> AsyncIterator[PcmChunk]:
        chunk_bytes = int(self._format.bytes_per_second * self._chunk_seconds)
        # round down to a whole frame so chunks never split a sample
        frame = self._format.channels * self._format.sample_width_bytes
        chunk_bytes -= chunk_bytes % frame
        silence = bytes(chunk_bytes)
        remaining = self._duration
        while remaining > 0:
            if self._realtime:
                await asyncio.sleep(min(self._chunk_seconds, remaining))
            yield PcmChunk(data=silence, format=self._format)
            remaining -= self._chunk_seconds
