"""Transport abstraction between STT clients and services.

The concrete transport is IN FLUX (direction of travel: WebSocket over a Unix
domain socket — PCM binary frames in, JSON transcript events out, one
connection). Per CLAUDE.md, nothing outside this module may assume a specific
transport: adapters implement ``SttService``, clients (harness, desktop
orchestrator) consume ``SttClient``/``SttSession``, and the wire protocol can
be swapped underneath without touching either side.

Two implementations are planned:

- ``LoopbackClient`` (here) — in-process, queue-based. Used by the Phase 0
  testbed and by contract tests; also the reference for session semantics.
- A WebSocket-over-UDS client/server pair (later) — the real IE114 transport,
  added once the spec settles. It must pass the same contract tests.

Session lifecycle, regardless of transport:

1. client opens a session with a ``SessionConfig``
2. client pushes ``PcmChunk``s; service emits events concurrently
3. client signals end of audio (``finish_audio``) — e.g. hotkey released
4. service flushes, emits remaining ``final`` events, then exactly one
   terminal event (``done`` or ``error``), and the event stream ends
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Awaitable, Callable
from typing import Protocol, runtime_checkable

from myna.core.audio import PcmChunk
from myna.core.capabilities import Capabilities
from myna.core.events import TranscriptionError, TranscriptionEvent
from myna.core.session import SessionConfig

EventSink = Callable[[TranscriptionEvent], Awaitable[None]]


@runtime_checkable
class SttSession(Protocol):
    """Client-side handle on one transcription session."""

    async def send_audio(self, chunk: PcmChunk) -> None: ...

    async def finish_audio(self) -> None:
        """Signal that no more audio will be sent (e.g. hotkey released)."""
        ...

    def events(self) -> AsyncIterator[TranscriptionEvent]:
        """Yield events until the terminal event (done/error) has been
        delivered, then stop."""
        ...

    async def aclose(self) -> None:
        """Abort the session, releasing resources. Idempotent."""
        ...


@runtime_checkable
class SttClient(Protocol):
    """Client-side factory for transcription sessions."""

    async def open_session(self, config: SessionConfig) -> SttSession: ...

    async def capabilities(self) -> Capabilities:
        """Query what the backend can do, before opening a session (T24)."""
        ...


@runtime_checkable
class SttService(Protocol):
    """Server-side handler for one transcription session. This is the
    interface every testbed adapter implements.

    Implementations must consume ``audio`` (which ends when the client
    finishes or aborts) and emit events via ``emit``, ending with exactly one
    terminal event. Model-specific messiness stays inside the implementation.
    """

    def capabilities(self) -> Capabilities:
        """Describe what this backend can do (T24). Static metadata — no I/O,
        no model load — so the client can discover it cheaply before a
        session."""
        ...

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None: ...


class LoopbackClient:
    """In-process ``SttClient`` that runs an ``SttService`` directly.

    No sockets, no serialization — used for Phase 0 contract verification and
    as the permanent regression fixture path (fake adapter + loopback).
    """

    def __init__(self, service: SttService) -> None:
        self._service = service

    async def open_session(self, config: SessionConfig) -> SttSession:
        return _LoopbackSession(self._service, config)

    async def capabilities(self) -> Capabilities:
        return self._service.capabilities()


class _LoopbackSession:
    _END = None  # queue sentinel

    def __init__(self, service: SttService, config: SessionConfig) -> None:
        self._audio: asyncio.Queue[PcmChunk | None] = asyncio.Queue()
        self._events: asyncio.Queue[TranscriptionEvent | None] = asyncio.Queue()
        self._audio_finished = False
        self._task = asyncio.ensure_future(self._run(service, config))

    async def _run(self, service: SttService, config: SessionConfig) -> None:
        async def audio_iter() -> AsyncIterator[PcmChunk]:
            while (chunk := await self._audio.get()) is not self._END:
                yield chunk

        try:
            await service.run_session(config, audio_iter(), self._events.put)
        except Exception as exc:  # adapter bug: surface as a terminal error event
            await self._events.put(
                TranscriptionError(code="adapter_crash", message=f"{type(exc).__name__}: {exc}")
            )
        finally:
            await self._events.put(self._END)

    async def send_audio(self, chunk: PcmChunk) -> None:
        if self._audio_finished:
            raise RuntimeError("send_audio() after finish_audio()")
        await self._audio.put(chunk)

    async def finish_audio(self) -> None:
        if not self._audio_finished:
            self._audio_finished = True
            await self._audio.put(self._END)

    async def events(self) -> AsyncIterator[TranscriptionEvent]:
        while (event := await self._events.get()) is not self._END:
            yield event

    async def aclose(self) -> None:
        await self.finish_audio()
        await self._task
