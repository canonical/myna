"""Audio sources for the harness.

All sources implement ``myna.core.AudioSource``. ``SilenceSource`` covers
contract tests; ``WavFileSource`` plays fixture clips (see
``myna.testbed.corpus``); ``MicSource`` captures live PipeWire audio (the
client side of the audio-push model, and the seed of the UD129 audio adapter,
T20); Phase 1 adds a virtual-PipeWire source on top.
"""

from __future__ import annotations

import asyncio
import wave
from collections.abc import AsyncIterator
from pathlib import Path

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


class WavFileSource:
    """Streams a PCM WAV file as chunks in its native format.

    ``realtime=True`` paces chunks at capture rate, mimicking live audio
    (Phase 1 lab runs); ``False`` streams as fast as the consumer accepts
    (batch accuracy runs). Resampling is not done here: candidates declare
    what they accept and adapters convert — the harness stays format-honest.

    Reads are synchronous (stdlib ``wave``); fixture clips are seconds long,
    so per-chunk reads are far below event-loop latency concerns.
    """

    def __init__(
        self,
        path: Path | str,
        *,
        chunk_seconds: float = 0.1,
        realtime: bool = False,
    ) -> None:
        self._path = Path(path)
        self._chunk_seconds = chunk_seconds
        self._realtime = realtime
        with wave.open(str(self._path), "rb") as wav:
            if wav.getcomptype() != "NONE":
                raise ValueError(f"{self._path}: only uncompressed PCM WAV is supported")
            self._format = AudioFormat(
                sample_rate_hz=wav.getframerate(),
                channels=wav.getnchannels(),
                sample_width_bytes=wav.getsampwidth(),
            )

    @property
    def format(self) -> AudioFormat:
        return self._format

    async def chunks(self) -> AsyncIterator[PcmChunk]:
        frames_per_chunk = max(1, round(self._format.sample_rate_hz * self._chunk_seconds))
        with wave.open(str(self._path), "rb") as wav:
            while data := wav.readframes(frames_per_chunk):
                chunk = PcmChunk(data=data, format=self._format)
                if self._realtime:
                    await asyncio.sleep(chunk.duration_seconds)
                yield chunk


class MicSource:
    """Live microphone capture via PipeWire (``pw-record --raw``).

    The client owns capture and pushes PCM to the STT service — the service
    never touches the microphone. Audio is streamed straight from the capture
    subprocess to chunks and never written to disk (UD129 privacy rule).

    Capture runs until ``stop()`` is called (e.g. on key release in a
    push-to-talk UI), at which point ``chunks()`` drains and exits so the
    harness can ``finish_audio()`` and the adapter can finalize. Mono S16LE at
    ``sample_rate_hz`` — the format Whisper wants, so no resampling needed.
    """

    def __init__(
        self,
        *,
        sample_rate_hz: int = 16_000,
        chunk_seconds: float = 0.1,
        target: str | None = None,
    ) -> None:
        self._format = AudioFormat(
            sample_rate_hz=sample_rate_hz, channels=1, sample_width_bytes=2
        )
        self._chunk_seconds = chunk_seconds
        self._target = target
        self._stop = asyncio.Event()

    @property
    def format(self) -> AudioFormat:
        return self._format

    def stop(self) -> None:
        """Signal capture to end; ``chunks()`` will then exit."""
        self._stop.set()

    async def chunks(self) -> AsyncIterator[PcmChunk]:
        cmd = [
            "pw-record", "--raw",
            "--rate", str(self._format.sample_rate_hz),
            "--channels", "1",
            "--format", "s16",
        ]
        if self._target:
            cmd += ["--target", self._target]
        cmd.append("-")  # write raw PCM to stdout

        frame = self._format.channels * self._format.sample_width_bytes
        n = int(self._format.bytes_per_second * self._chunk_seconds)
        n -= n % frame  # whole frames only

        proc = await asyncio.create_subprocess_exec(
            *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
        )
        try:
            while not self._stop.is_set():
                try:
                    # bounded wait so stop() is honoured promptly between reads
                    data = await asyncio.wait_for(proc.stdout.read(n), timeout=0.25)
                except asyncio.TimeoutError:
                    continue
                if not data:
                    break
                yield PcmChunk(data=data, format=self._format)
        finally:
            proc.terminate()
            await proc.wait()
