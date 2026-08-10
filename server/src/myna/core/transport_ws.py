"""WebSocket-over-Unix-domain-socket transport (T16 prototype).

Implements the session contract from ``myna.core.transport`` over the
direction-of-travel IE114 transport: one WebSocket connection per session,
PCM as binary frames in, JSON transcript events as text frames out.

Wire protocol (PROVISIONAL — input to the IE115 reconciliation, T18/T35):

1. Client connects to the Unix socket and completes the WebSocket handshake.
   The server immediately sends one ``session.created`` **greeting** (the
   OpenAI-Realtime pattern: server speaks first) carrying both the served
   ``protocol_version`` (additive, for version-aware clients) and the IE115
   ``session`` defaults (for stock OpenAI clients, which wait for
   ``session.created`` before sending anything — without the greeting they
   would deadlock against the shape-sniff below).
2. Client sends one text frame. Either:
   - ``{"type": "capabilities.query"}`` — the server replies with one
     ``{"type": "capabilities", "data": {...}}`` frame and closes (T24); or
   - ``{"type": "session.start", "protocol_version": "1", "config": {...}}`` to
     open a session, where ``config`` is the ``SessionConfig`` wire form (audio
     format, language, prompt, ...), continuing below.
3. Server checks the declared ``protocol_version`` (``myna.core.protocol``). If
   it can't serve it, the server sends a terminal
   ``transcription.error`` (code ``unsupported_protocol_version``) and closes.
   The greeting already told the client which version the server speaks; there
   is no per-session ack beyond it. (No ``session.updated`` on this dialect:
   there is no mid-session reconfig.)
4. Client streams raw PCM as binary frames, in the declared audio format.
   (Binary frames, deliberately not IE115's base64-in-JSON — see
   ``docs/IE115-deviations.md`` §1.3. Turn detection is client-driven: there is
   no server VAD; the client owns the boundary.)
5. Client sends a text frame ``{"type": "session.finish"}`` when audio ends
   (hotkey released). Closing the connection instead aborts the session.
6. Server sends transcript events as text frames in the
   ``{"event": ..., "data": {...}}`` shape (see ``myna.core.events``), ending
   with exactly one terminal event, then closes the connection. Control frames
   (``session.created``) carry a ``"type"`` key; transcript events carry an
   ``"event"`` key — that is how the client tells them apart.

Like every transport, this module must pass ``tests/test_contract.py`` with
only the wiring swapped — the loopback transport is the reference semantics.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import socket
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any, cast

from websockets.asyncio.client import ClientConnection, unix_connect
from websockets.asyncio.server import Server, ServerConnection, unix_serve
from websockets.exceptions import ConnectionClosed

from myna.core.audio import AudioFormat, PcmChunk
from myna.core.capabilities import (
    Capabilities,
    capabilities_from_wire,
    capabilities_to_wire,
)
from myna.core.events import (
    TranscriptionError,
    TranscriptionEvent,
    event_from_wire,
    event_to_wire,
)
from myna.core.protocol import (
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
    is_supported,
)
from myna.core.session import (
    SessionConfig,
    session_config_from_wire,
    session_config_to_wire,
)
from myna.core.transport import EventSink, SttService
from myna.core.wire_ie115 import (
    INPUT_AUDIO_APPEND,
    INPUT_AUDIO_COMMIT,
    SESSION_CREATED,
    SESSION_UPDATE,
    SESSION_UPDATED,
    Ie115Decoder,
    Ie115Encoder,
    append_to_pcm,
    pcm_to_append,
    session_config_from_ie115,
    session_config_to_ie115,
)

_TERMINAL = ("transcription.done", "transcription.error")

# Bound on the audio backlog the server holds per connection, in chunks
# (~26 s at the client's ~100 ms chunks). Bounded, not unbounded, per the
# bounded-buffer invariant: when a slow adapter falls behind on long-form
# dictation, `put` blocks the WS reader, and backpressure propagates to the
# client through the socket instead of growing server memory. This queue is
# also where a future overload/lag signal (open item, Matias) would be
# measured — design that signal against this bound.
_AUDIO_QUEUE_MAXSIZE = 256


_SD_LISTEN_FDS_START = 3  # systemd passes listening sockets from fd 3 up


def systemd_socket() -> socket.socket | None:
    """The listening socket handed in by systemd/snapd socket activation, or
    ``None`` if this process wasn't launched that way.

    Under socket activation, systemd owns and binds the socket and passes it as
    fd 3 (advertised via ``LISTEN_FDS``/``LISTEN_PID``); the daemon serves on it
    instead of binding a path, so it can exit on idle and be relaunched on the
    next connection (T28). Testable in dev with ``systemd-socket-activate``.
    """
    if os.environ.get("LISTEN_PID") != str(os.getpid()):
        return None
    try:
        count = int(os.environ.get("LISTEN_FDS", "0"))
    except ValueError:
        return None
    if count < 1:
        return None
    return socket.socket(
        fileno=_SD_LISTEN_FDS_START, family=socket.AF_UNIX, type=socket.SOCK_STREAM
    )


@contextlib.asynccontextmanager
async def serve_unix(
    service: SttService,
    socket_path: Path | str | None = None,
    *,
    sock: socket.socket | None = None,
) -> AsyncIterator[Server]:
    """Serve ``service`` on a Unix socket; one WebSocket connection per
    session. Use as an async context manager. Either binds ``socket_path``, or
    serves on a pre-bound ``sock`` (e.g. from ``systemd_socket()``)."""
    handler = _SessionHandler(service)
    if sock is not None:
        cm = unix_serve(handler.handle, sock=sock)
        async with cast(contextlib.AbstractAsyncContextManager[Server], cm) as server:
            yield server
    else:
        cm = unix_serve(handler.handle, path=str(socket_path))
        async with cast(contextlib.AbstractAsyncContextManager[Server], cm) as server:
            yield server


class _SessionHandler:
    def __init__(self, service: SttService) -> None:
        self._service = service

    async def handle(self, ws: ServerConnection) -> None:
        """One connection. The server speaks first: one ``session.created``
        greeting (server defaults + served ``protocol_version``) goes out on
        connect, so a stock OpenAI client — which waits for ``session.created``
        before sending anything — cannot deadlock against the shape-sniff.
        The dialect is then chosen by **shape-sniffing** the client's first
        frame (T44/T45): ``capabilities.query`` -> capabilities;
        ``session.start`` -> internal ``myna.core`` wire; ``session.update`` ->
        IE115 dialect. Both dialects' clients skip the greeting as a control
        frame."""
        caps = self._service.capabilities()
        default_model = caps.models[0] if caps.models else None
        default_format = caps.input_formats[0] if caps.input_formats else AudioFormat()
        # T027: advertise streaming mode on the greeting (additive; absent on
        # adapters that don't define `streaming` → old behavior).
        streaming = getattr(self._service, "streaming", None)
        with contextlib.suppress(ConnectionClosed):
            await ws.send(
                json.dumps(
                    {
                        "type": SESSION_CREATED,
                        "protocol_version": PROTOCOL_VERSION,
                        "session": session_config_to_ie115(
                            SessionConfig(audio_format=default_format),
                            model=default_model,
                            streaming=streaming,
                        ),
                    }
                )
            )
        try:
            opening = await ws.recv()
        except ConnectionClosed:
            return
        if self._is_capabilities_query(opening):
            wire = capabilities_to_wire(caps)
            with contextlib.suppress(ConnectionClosed):
                await ws.send(json.dumps({"type": "capabilities", "data": wire}))
                await ws.close()
            return

        update = self._parse_session_update(opening)
        if update is not None:
            await self._handle_ie115(ws, *update, caps=caps)
            return

        start = self._parse_start(opening)
        if start is None:
            await ws.close(
                code=1002,
                reason="expected session.start, session.update, or capabilities.query",
            )
            return
        await self._handle_internal(ws, *start)

    async def _handle_internal(
        self, ws: ServerConnection, config: SessionConfig, client_version: str | None
    ) -> None:
        if not is_supported(client_version):
            with contextlib.suppress(ConnectionClosed):
                await ws.send(
                    json.dumps(
                        event_to_wire(
                            TranscriptionError(
                                code="unsupported_protocol_version",
                                message=(
                                    f"server speaks {sorted(SUPPORTED_PROTOCOL_VERSIONS)}, "
                                    f"client requested {client_version!r}"
                                ),
                            )
                        )
                    )
                )
                await ws.close()
            return

        def on_text(message: dict[str, Any], config: SessionConfig) -> tuple[str, PcmChunk | None]:
            return ("finish", None) if message.get("type") == "session.finish" else ("ignore", None)

        async def emit(event: TranscriptionEvent) -> None:
            with contextlib.suppress(ConnectionClosed):
                await ws.send(json.dumps(event_to_wire(event)))

        await self._pump_session(ws, config, on_text=on_text, emit=emit)

    async def _handle_ie115(
        self,
        ws: ServerConnection,
        config: SessionConfig,
        requested_model: str | None,
        *,
        caps: Capabilities,
    ) -> None:
        """IE115 dialect: OpenAI-shaped frames translated via ``wire_ie115``.
        The ``session.created`` greeting already went out in ``handle``.

        The connection is **persistent** (OpenAI multi-commit shape, decided
        2026-07-06): each ``input_audio_buffer.commit`` closes one utterance —
        one adapter session, terminated by its ``completed`` — and the
        connection stays open for the next. The *client* closes when it is
        finished; the server only closes after the client does."""
        encoder = Ie115Encoder()

        # One model per process: a request for a model this server does not
        # serve is REJECTED, never silently answered by a different model (a
        # compat client asking for X must not get Y). Selecting *which* server
        # to dial is the discovery layer's job (plan T48), not this session's.
        if requested_model is not None and requested_model not in caps.models:
            with contextlib.suppress(ConnectionClosed):
                await ws.send(
                    json.dumps(
                        encoder.encode(
                            TranscriptionError(
                                code="model_not_available",
                                message=(
                                    f"this server serves {list(caps.models)!r}, "
                                    f"not {requested_model!r}"
                                ),
                            )
                        )
                    )
                )
                await ws.close()
            return
        model = requested_model or (caps.models[0] if caps.models else None)

        with contextlib.suppress(ConnectionClosed):
            await ws.send(
                json.dumps(
                    {
                        "type": SESSION_UPDATED,
                        "session": session_config_to_ie115(config, model=model),
                    }
                )
            )

        # One reader for the whole connection; _COMMIT marks utterance
        # boundaries in-band, None marks the client closing the connection.
        _COMMIT = object()
        frames: asyncio.Queue[PcmChunk | object | None] = asyncio.Queue(_AUDIO_QUEUE_MAXSIZE)

        async def read_frames() -> None:
            try:
                async for frame in ws:
                    if isinstance(frame, bytes):
                        await frames.put(PcmChunk(data=frame, format=config.audio_format))
                        continue
                    message = json.loads(frame)
                    mtype = message.get("type")
                    if mtype == INPUT_AUDIO_APPEND:
                        await frames.put(append_to_pcm(message, config.audio_format))
                    elif mtype == INPUT_AUDIO_COMMIT:
                        await frames.put(_COMMIT)
                    # other client frames (e.g. further session.update): ignored
            except ConnectionClosed:
                pass
            finally:
                await frames.put(None)

        class _Utterance:
            """Audio of one commit cycle, tracking whether its boundary
            (commit or close) has been consumed off the queue yet."""

            def __init__(self) -> None:
                self.ended = False
                self.closed = False

            async def audio(self) -> AsyncIterator[PcmChunk]:
                while True:
                    item = await frames.get()
                    if item is _COMMIT or item is None:
                        self.ended = True
                        self.closed = item is None
                        return
                    if isinstance(item, PcmChunk):
                        yield item

            async def drain(self) -> None:
                """Consume to the boundary if the adapter stopped pulling early
                (e.g. rejected the format) so leftover audio of this utterance
                is discarded, not misread as the next one."""
                while not self.ended:
                    item = await frames.get()
                    if item is _COMMIT or item is None:
                        self.ended = True
                        self.closed = item is None

        reader = asyncio.ensure_future(read_frames())
        try:
            while True:
                # Start the adapter session EAGERLY, before any audio arrives:
                # the adapter's load-heartbeat and `ready` must flow first,
                # because a well-behaved client gates its audio on `ready`
                # (lifecycle §3A / T42 — waiting for audio here would deadlock
                # against a client waiting for `ready`). The cost is one empty
                # adapter run when the client closes between utterances; its
                # sends are suppressed and an empty utterance is cheap.
                utterance = _Utterance()
                terminal_seen = False

                async def emit(event: TranscriptionEvent) -> None:
                    nonlocal terminal_seen
                    if event.type in _TERMINAL:
                        terminal_seen = True
                    with contextlib.suppress(ConnectionClosed):
                        await ws.send(json.dumps(encoder.encode(event)))

                await self._run_utterance(config, utterance.audio(), emit)
                await utterance.drain()
                if utterance.closed:
                    break  # client closed the connection: normal end
                if not terminal_seen:
                    # Adapter broke the exactly-one-terminal contract; a compat
                    # client is now waiting on `completed` — fail the utterance
                    # rather than hang it.
                    await emit(
                        TranscriptionError(
                            code="internal",
                            message="adapter ended without a terminal event",
                        )
                    )
        finally:
            reader.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await reader
            await ws.close()

    async def _pump_session(
        self,
        ws: ServerConnection,
        config: SessionConfig,
        *,
        on_text: Callable[[dict[str, Any], SessionConfig], tuple[str, PcmChunk | None]],
        emit: EventSink,
    ) -> None:
        """Internal-dialect session loop (one utterance per connection, server
        closes after the terminal event): binary frames -> PCM; text frames
        dispatched by ``on_text`` (finish/ignore); events out via ``emit``.
        The reader runs concurrently with the adapter (commit-drain)."""
        audio: asyncio.Queue[PcmChunk | None] = asyncio.Queue(_AUDIO_QUEUE_MAXSIZE)

        async def read_frames() -> None:
            try:
                async for frame in ws:
                    if isinstance(frame, bytes):
                        await audio.put(PcmChunk(data=frame, format=config.audio_format))
                        continue
                    kind, chunk = on_text(json.loads(frame), config)
                    if kind == "append" and chunk is not None:
                        await audio.put(chunk)
                    elif kind == "finish":
                        break
            except ConnectionClosed:
                pass  # client abort: just end the audio stream
            finally:
                await audio.put(None)

        async def audio_iter() -> AsyncIterator[PcmChunk]:
            while (chunk := await audio.get()) is not None:
                yield chunk

        reader = asyncio.ensure_future(read_frames())
        try:
            await self._run_utterance(config, audio_iter(), emit)
        finally:
            reader.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await reader
            await ws.close()

    async def _run_utterance(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        """Run one adapter session, surfacing adapter bugs as a terminal error
        event (shared by both dialects)."""
        try:
            await self._service.run_session(config, audio, emit)
        except Exception as exc:
            await emit(
                TranscriptionError(code="adapter_crash", message=f"{type(exc).__name__}: {exc}")
            )

    @staticmethod
    def _is_capabilities_query(frame: str | bytes) -> bool:
        if isinstance(frame, bytes):
            return False
        try:
            return bool(json.loads(frame).get("type") == "capabilities.query")
        except (ValueError, TypeError, AttributeError):
            return False

    @staticmethod
    def _parse_start(frame: str | bytes) -> tuple[SessionConfig, str | None] | None:
        """Parse a ``session.start`` into ``(config, protocol_version)``, or
        ``None`` if the frame isn't a session.start. ``protocol_version`` is
        ``None`` when the client omits it (pre-versioning clients)."""
        if isinstance(frame, bytes):
            return None
        try:
            message = json.loads(frame)
            if message.get("type") != "session.start":
                return None
            config = session_config_from_wire(message.get("config") or {})
            return config, message.get("protocol_version")
        except (ValueError, TypeError):
            return None

    @staticmethod
    def _parse_session_update(
        frame: str | bytes,
    ) -> tuple[SessionConfig, str | None] | None:
        """Parse an IE115 ``session.update`` into ``(config, requested_model)``,
        or ``None`` if the frame isn't one (the shape-sniff hook). The model is
        kept separate from the flat config: it names *which server* should
        answer, not how this session decodes."""
        if isinstance(frame, bytes):
            return None
        try:
            message = json.loads(frame)
            if message.get("type") != SESSION_UPDATE:
                return None
            session = message.get("session") or {}
            transcription = (((session.get("audio") or {}).get("input")) or {}).get(
                "transcription"
            ) or {}
            return session_config_from_ie115(session), transcription.get("model")
        except (ValueError, TypeError):
            return None


class WsUnixClient:
    """``SttClient`` over WebSocket on a Unix socket."""

    def __init__(self, socket_path: Path | str) -> None:
        self._socket_path = str(socket_path)

    async def open_session(self, config: SessionConfig) -> _WsSession:
        ws = await unix_connect(self._socket_path)
        await ws.send(
            json.dumps(
                {
                    "type": "session.start",
                    "protocol_version": PROTOCOL_VERSION,
                    "config": session_config_to_wire(config),
                }
            )
        )
        return _WsSession(ws)

    async def capabilities(self) -> Capabilities:
        ws = await unix_connect(self._socket_path)
        try:
            await ws.send(json.dumps({"type": "capabilities.query"}))
            reply = json.loads(await ws.recv())
            if reply.get("type") == SESSION_CREATED:  # the server's greeting
                reply = json.loads(await ws.recv())
            if reply.get("type") != "capabilities":
                raise ValueError(f"expected capabilities reply, got {reply.get('type')!r}")
            return capabilities_from_wire(reply.get("data") or {})
        finally:
            await ws.close()


class _WsSession:
    def __init__(self, ws: ClientConnection) -> None:
        self._ws = ws
        self._audio_finished = False
        self.protocol_version: str | None = None  # set from session.created

    async def send_audio(self, chunk: PcmChunk) -> None:
        if self._audio_finished:
            raise RuntimeError("send_audio() after finish_audio()")
        # The server may end the session (terminal error/done) and close mid-
        # stream; the reason arrives on the events channel. A failed send here is
        # then expected — suppress it (as finish_audio does) so it doesn't mask
        # the real error with a ConnectionClosed traceback from the feeder.
        with contextlib.suppress(ConnectionClosed):
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
                message = json.loads(frame)
                if message.get("type") == "session.created":
                    self.protocol_version = message.get("protocol_version")
                    continue  # the server's greeting, not a transcript event
                if "event" not in message:
                    continue  # other control frames: ignore
                event = event_from_wire(message)
                if event is None:
                    continue  # unknown event type: additive, skip
                yield event
                if event.type in _TERMINAL:
                    return
        except ConnectionClosed:
            return  # session ended without a terminal event (server abort)

    async def aclose(self) -> None:
        await self._ws.close()


class WsUnixIe115Client:
    """``SttClient`` speaking the **IE115 dialect** over ws+unix (T45 reference
    client; the Rust client mirrors it, T43). Sends ``session.update`` eagerly as
    the first frame (shape-sniff trigger). Audio goes as raw binary frames by
    default, or base64 ``input_audio_buffer.append`` when ``base64_audio=True``
    (OpenAI-parity path, feeds the T46 micro-benchmark)."""

    def __init__(self, socket_path: Path | str, *, base64_audio: bool = False) -> None:
        self._socket_path = str(socket_path)
        self._base64_audio = base64_audio

    async def open_session(self, config: SessionConfig) -> _Ie115Session:
        ws = await unix_connect(self._socket_path)
        await ws.send(
            json.dumps({"type": SESSION_UPDATE, "session": session_config_to_ie115(config)})
        )
        return _Ie115Session(ws, base64_audio=self._base64_audio)


class _Ie115Session:
    def __init__(self, ws: ClientConnection, *, base64_audio: bool) -> None:
        self._ws = ws
        self._base64_audio = base64_audio
        self._audio_finished = False
        self._decoder = Ie115Decoder()

    async def send_audio(self, chunk: PcmChunk) -> None:
        if self._audio_finished:
            raise RuntimeError("send_audio() after finish_audio()")
        # See _WsSession.send_audio: tolerate the server closing mid-stream so a
        # terminal error isn't masked by a ConnectionClosed traceback.
        with contextlib.suppress(ConnectionClosed):
            if self._base64_audio:
                await self._ws.send(json.dumps(pcm_to_append(chunk)))
            else:
                await self._ws.send(chunk.data)

    async def finish_audio(self) -> None:
        if not self._audio_finished:
            self._audio_finished = True
            with contextlib.suppress(ConnectionClosed):
                await self._ws.send(json.dumps({"type": INPUT_AUDIO_COMMIT}))

    async def events(self) -> AsyncIterator[TranscriptionEvent]:
        try:
            async for frame in self._ws:
                if isinstance(frame, bytes):
                    continue  # server never sends binary; tolerate it
                for event in self._decoder.decode(json.loads(frame)):
                    yield event
                    if event.type in _TERMINAL:
                        return
        except ConnectionClosed:
            pass  # fall through: synthesise the terminal from close
        for event in self._decoder.on_close():  # IE115 has no session `done` frame
            yield event

    async def aclose(self) -> None:
        await self._ws.close()
