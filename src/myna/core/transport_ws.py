"""WebSocket-over-Unix-domain-socket transport (T16 prototype).

Implements the session contract from ``myna.core.transport`` over the
direction-of-travel IE114 transport: one WebSocket connection per session,
PCM as binary frames in, JSON transcript events as text frames out.

Wire protocol (PROVISIONAL — input to the IE114 update, T18):

1. Client connects to the Unix socket and completes the WebSocket handshake.
2. Client sends one text frame: ``{"type": "session.start", "config": {...}}``
   where ``config`` is the ``SessionConfig`` wire form (audio format,
   language, prompt, ...).
3. Client streams raw PCM as binary frames, in the declared audio format.
4. Client sends a text frame ``{"type": "session.finish"}`` when audio ends
   (hotkey released). Closing the connection instead aborts the session.
5. Server sends transcript events as text frames in the
   ``{"event": ..., "data": {...}}`` shape (see ``myna.core.events``), ending
   with exactly one terminal event, then closes the connection.

Like every transport, this module must pass ``tests/test_contract.py`` with
only the wiring swapped — the loopback transport is the reference semantics.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
from collections.abc import AsyncIterator
from pathlib import Path

from websockets.asyncio.client import unix_connect
from websockets.asyncio.server import ServerConnection, unix_serve
from websockets.exceptions import ConnectionClosed

from myna.core.audio import PcmChunk
from myna.core.events import TranscriptionError, TranscriptionEvent, event_from_wire, event_to_wire
from myna.core.session import SessionConfig, session_config_from_wire, session_config_to_wire
from myna.core.transport import SttService

_TERMINAL = ("transcription.done", "transcription.error")


@contextlib.asynccontextmanager
async def serve_unix(service: SttService, socket_path: Path | str):
    """Serve ``service`` on a Unix socket; one WebSocket connection per
    session. Use as an async context manager."""
    handler = _SessionHandler(service)
    async with unix_serve(handler.handle, path=str(socket_path)) as server:
        yield server


class _SessionHandler:
    def __init__(self, service: SttService) -> None:
        self._service = service

    async def handle(self, ws: ServerConnection) -> None:
        try:
            opening = await ws.recv()
        except ConnectionClosed:
            return
        config = self._parse_start(opening)
        if config is None:
            await ws.close(code=1002, reason="expected session.start")
            return

        audio: asyncio.Queue[PcmChunk | None] = asyncio.Queue()

        async def read_frames() -> None:
            try:
                async for frame in ws:
                    if isinstance(frame, bytes):
                        await audio.put(PcmChunk(data=frame, format=config.audio_format))
                    elif json.loads(frame).get("type") == "session.finish":
                        break
            except ConnectionClosed:
                pass  # client abort: just end the audio stream
            finally:
                await audio.put(None)

        async def audio_iter() -> AsyncIterator[PcmChunk]:
            while (chunk := await audio.get()) is not None:
                yield chunk

        async def emit(event: TranscriptionEvent) -> None:
            with contextlib.suppress(ConnectionClosed):
                await ws.send(json.dumps(event_to_wire(event)))

        reader = asyncio.ensure_future(read_frames())
        try:
            await self._service.run_session(config, audio_iter(), emit)
        except Exception as exc:  # adapter bug: surface as a terminal error event
            await emit(
                TranscriptionError(code="adapter_crash", message=f"{type(exc).__name__}: {exc}")
            )
        finally:
            reader.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await reader
            await ws.close()

    @staticmethod
    def _parse_start(frame: str | bytes) -> SessionConfig | None:
        if isinstance(frame, bytes):
            return None
        try:
            message = json.loads(frame)
            if message.get("type") != "session.start":
                return None
            return session_config_from_wire(message.get("config") or {})
        except (ValueError, TypeError):
            return None


class WsUnixClient:
    """``SttClient`` over WebSocket on a Unix socket."""

    def __init__(self, socket_path: Path | str) -> None:
        self._socket_path = str(socket_path)

    async def open_session(self, config: SessionConfig) -> "_WsSession":
        ws = await unix_connect(self._socket_path)
        await ws.send(
            json.dumps({"type": "session.start", "config": session_config_to_wire(config)})
        )
        return _WsSession(ws)


class _WsSession:
    def __init__(self, ws) -> None:
        self._ws = ws
        self._audio_finished = False

    async def send_audio(self, chunk: PcmChunk) -> None:
        if self._audio_finished:
            raise RuntimeError("send_audio() after finish_audio()")
        await self._ws.send(chunk.data)

    async def finish_audio(self) -> None:
        if not self._audio_finished:
            self._audio_finished = True
            with contextlib.suppress(ConnectionClosed):
                await self._ws.send(json.dumps({"type": "session.finish"}))

    async def events(self) -> AsyncIterator[TranscriptionEvent]:
        try:
            async for frame in self._ws:
                if isinstance(frame, bytes):
                    continue  # server never sends binary; tolerate it
                event = event_from_wire(json.loads(frame))
                yield event
                if event.type in _TERMINAL:
                    return
        except ConnectionClosed:
            return  # session ended without a terminal event (server abort)

    async def aclose(self) -> None:
        await self._ws.close()
