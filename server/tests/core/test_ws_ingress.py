"""Server ingress on both dialects: the path from a client's audio frames to
the adapter's audio iterator when the adapter is slow, fails or is cancelled,
and when the client or the server goes away with the audio queue full.

Every scenario runs its own event loop on a daemon thread: a hung server also
hangs ``serve_unix``'s teardown and ``asyncio.run``'s task cancellation, which
no timeout inside the loop can bound.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import socket
import threading
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
from pathlib import Path
from typing import Any

import pytest
from websockets.asyncio.client import ClientConnection, unix_connect
from websockets.exceptions import ConnectionClosed

from myna.core import (
    AudioFormat,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    serve_unix,
)
from myna.core import wire_ie115 as w
from myna.core.protocol import PROTOCOL_VERSION
from myna.core.session import session_config_to_wire
from myna.core.transport_ws import _BOUNDARY, _Ingress
from myna.testbed import FakeAdapter

BOUND = 5.0
SECOND = bytes(range(256)) * 125  # one second of 16 kHz PCM
# Many times what the server buffers plus what the kernel absorbs, so the
# sender stalls unless the server keeps reading; see open_session.
FLOOD = 512
DIALECTS = ("internal", "ie115")


class Scenario:
    """Runs one scenario on its own loop and thread. An assertion failing
    inside ``serving`` is recorded before the server's teardown, which may be
    the very thing that hangs."""

    def __init__(self, tmp_path: Path) -> None:
        self.path = tmp_path / "myna.sock"
        self.errors: list[BaseException] = []
        self._settled = threading.Event()

    @contextlib.asynccontextmanager
    async def serving(self, adapter: Any) -> AsyncIterator[None]:
        async with serve_unix(adapter, self.path):
            try:
                yield
            except BaseException as exc:
                self.errors.append(exc)
                self._settled.set()
                raise

    def run(self, main: Callable[[], Awaitable[Any]], timeout: float = 3 * BOUND) -> None:
        def target() -> None:
            try:
                asyncio.run(main())
            except BaseException as exc:
                self.errors.append(exc)
            finally:
                self._settled.set()

        thread = threading.Thread(target=target, daemon=True)
        thread.start()
        self._settled.wait(timeout)
        if self.errors:
            raise self.errors[0]
        thread.join(BOUND)
        assert not thread.is_alive(), "the server hung"


@pytest.fixture
def scenario(tmp_path: Path) -> Scenario:
    return Scenario(tmp_path)


async def open_session(path: Path, dialect: str, rate: int = 16_000) -> ClientConnection:
    # A small send buffer keeps the kernel from absorbing megabytes of audio
    # the server has not read, so a stalled reader shows as a stalled sender.
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
    sock.connect(str(path))
    ws = await unix_connect(sock=sock, ping_interval=None)
    assert json.loads(await ws.recv())["type"] == w.SESSION_CREATED
    config = SessionConfig(audio_format=AudioFormat(sample_rate_hz=rate))
    if dialect == "internal":
        start = {
            "type": "session.start",
            "protocol_version": PROTOCOL_VERSION,
            "config": session_config_to_wire(config),
        }
        await ws.send(json.dumps(start))
    else:
        update = {"type": w.SESSION_UPDATE, "session": w.session_config_to_ie115(config)}
        await ws.send(json.dumps(update))
        assert json.loads(await ws.recv())["type"] == w.SESSION_UPDATED
    return ws


def finish_frame(dialect: str) -> str:
    kind = "session.finish" if dialect == "internal" else w.INPUT_AUDIO_COMMIT
    return json.dumps({"type": kind})


async def send_all(ws: ClientConnection, frames: Iterable[bytes | str]) -> None:
    with contextlib.suppress(ConnectionClosed):
        for frame in frames:
            await ws.send(frame)


async def stalls(task: asyncio.Task[Any], settle: float = 0.5) -> bool:
    """True if ``task`` is still blocked after ``settle`` seconds: megabytes of
    audio take milliseconds to send unless the server stops reading."""
    done, _ = await asyncio.wait({task}, timeout=settle)
    return not done


async def terminal(ws: ClientConnection) -> dict[str, Any]:
    """The next terminal frame of either dialect."""
    while True:
        frame = json.loads(await asyncio.wait_for(ws.recv(), BOUND))
        if frame.get("event") in ("transcription.done", "transcription.error"):
            return frame
        if frame.get("type") in (w.TRANSCRIPTION_COMPLETED, w.ERROR):
            return frame


class Gated:
    """Consumes no audio until released, then hands the session to ``then``.
    Keeps what each session heard, chunk by chunk."""

    def __init__(
        self, then: Callable[[Gated, AsyncIterator[PcmChunk], EventSink], Awaitable[None]]
    ) -> None:
        self._then = then
        self.release = asyncio.Event()
        self.ended = asyncio.Event()
        self.sessions: list[list[bytes]] = []

    def capabilities(self):
        return FakeAdapter().capabilities()

    async def run_session(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        self.sessions.append([])
        await self.release.wait()
        await self._then(self, audio, emit)

    async def record(self, audio: AsyncIterator[PcmChunk]) -> None:
        heard = self.sessions[-1]
        async for chunk in audio:
            heard.append(chunk.data)
        self.ended.set()


async def record_then_done(adapter: Gated, audio: AsyncIterator[PcmChunk], emit: EventSink) -> None:
    await adapter.record(audio)
    await emit(TranscriptionDone(text=""))


async def fail(adapter: Gated, audio: AsyncIterator[PcmChunk], emit: EventSink) -> None:
    await emit(TranscriptionError(code="inference_failed", message="out of memory"))


async def cancel_own_task(adapter: Gated, audio: AsyncIterator[PcmChunk], emit: EventSink) -> None:
    task = asyncio.current_task()
    assert task is not None
    task.cancel()
    await asyncio.sleep(0)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_an_adapter_failing_with_a_full_queue_releases_the_reader(dialect, scenario):
    """The adapter stops consuming and fails while the client is still feeding:
    the reader, blocked on the full queue, must be released so the client's
    sends complete and the connection can end."""
    path = scenario.path

    async def main() -> None:
        adapter = Gated(fail)
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect)
            feed = asyncio.create_task(send_all(ws, [*[SECOND] * FLOOD, finish_frame(dialect)]))
            assert await stalls(feed), "the queue never filled"
            adapter.release.set()
            assert (await terminal(ws)).get("event", "error") in ("transcription.error", "error")
            await asyncio.wait_for(feed, BOUND)
            await ws.close()

    scenario.run(main)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_a_session_cancelled_with_a_full_queue_still_closes(dialect, scenario):
    """Regression: the IE115 reader, cancelled with the queue full, parked in
    ``finally: put(None)`` on a queue nobody would drain again, so the handler
    never closed the connection and the server never shut down."""
    path = scenario.path

    async def main() -> None:
        adapter = Gated(cancel_own_task)
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect)
            feed = asyncio.create_task(send_all(ws, [SECOND] * FLOOD))
            assert await stalls(feed), "the queue never filled"
            adapter.release.set()
            await asyncio.wait_for(ws.wait_closed(), BOUND)
            await asyncio.wait_for(feed, BOUND)

    scenario.run(main)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_shutdown_aborts_a_full_queue_instead_of_draining_it(dialect, scenario):
    """Server shutdown is an abort: queued audio is discarded, the adapter's
    audio ends at once, and the server does not wait for a slow adapter to
    work through a backlog nobody will hear."""
    path = scenario.path

    async def main() -> None:
        adapter = Gated(record_then_done)
        server = serve_unix(adapter, path)
        await server.__aenter__()
        ws = await open_session(path, dialect)
        feed = asyncio.create_task(send_all(ws, [SECOND] * FLOOD))
        assert await stalls(feed), "the queue never filled"
        shutdown = asyncio.create_task(server.__aexit__(None, None, None))
        await asyncio.sleep(0)  # the shutdown's first step runs before the release
        adapter.release.set()
        await asyncio.wait_for(shutdown, BOUND)
        await asyncio.wait_for(feed, BOUND)
        assert adapter.sessions == [[]]

    scenario.run(main)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_a_client_vanishing_with_a_full_queue_ends_the_adapter(dialect, scenario):
    path = scenario.path

    async def main() -> None:
        adapter = Gated(record_then_done)
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect)
            feed = asyncio.create_task(send_all(ws, [SECOND] * FLOOD))
            assert await stalls(feed), "the queue never filled"
            ws.transport.abort()  # no close handshake: the process died
            adapter.release.set()
            await asyncio.wait_for(adapter.ended.wait(), BOUND)
            await asyncio.wait_for(feed, BOUND)

    scenario.run(main)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_a_clean_close_mid_utterance_ends_the_adapter(dialect, scenario):
    path = scenario.path

    async def main() -> None:
        adapter = Gated(record_then_done)
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect)
            await ws.send(SECOND)
            await ws.close()
            adapter.release.set()
            await asyncio.wait_for(adapter.ended.wait(), BOUND)

    scenario.run(main)


def test_an_undecodable_internal_control_frame_ends_the_audio_and_is_logged(scenario, caplog):
    path = scenario.path

    async def main() -> None:
        async with scenario.serving(FakeAdapter()):
            ws = await open_session(path, "internal")
            await ws.send(SECOND)
            await ws.send("not json")
            assert (await terminal(ws))["event"] == "transcription.done"
            await asyncio.wait_for(ws.wait_closed(), BOUND)

    scenario.run(main)
    assert "ingress reader failed" in caplog.text


def partition(pcm: bytes, cuts: tuple[int, ...] = (1, 3200, 3, 1001, 2, 4799)) -> list[bytes]:
    pieces, at, i = [], 0, 0
    while at < len(pcm):
        pieces.append(pcm[at : at + cuts[i % len(cuts)]])
        at += cuts[i % len(cuts)]
        i += 1
    return pieces


def ramp(rate: int, seconds: float) -> bytes:
    return bytes(i * 7 % 256 for i in range(int(rate * seconds) * 2))


@pytest.mark.parametrize(
    ("dialect", "rate"), [("internal", 16_000), ("ie115", 16_000), ("ie115", 24_000)]
)
def test_odd_byte_appends_reach_the_adapter_in_whole_samples(dialect, rate, scenario, caplog):
    """However the client cuts its appends, the adapter sees whole samples,
    the same ones the unsplit input yields; a trailing half sample at the
    finish is dropped with a warning, never padded."""
    from myna.core.resample import Resampler

    path = scenario.path
    pcm = ramp(rate, 0.4)
    whole = Resampler(rate, 16_000)
    expected = whole.feed(pcm) + whole.flush()

    async def main() -> None:
        adapter = Gated(record_then_done)
        adapter.release.set()
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect, rate)
            for piece in partition(pcm + b"\x01"):
                await ws.send(piece)
            await ws.send(finish_frame(dialect))
            assert (await terminal(ws)).get("type") != w.ERROR
            await ws.close()
        heard = adapter.sessions[0]
        assert all(len(chunk) % 2 == 0 for chunk in heard)
        assert b"".join(heard) == expected

    scenario.run(main)
    assert "1 byte" in caplog.text


def test_ie115_commit_does_not_carry_a_half_sample_into_the_next_utterance(scenario):
    path = scenario.path
    first, second = ramp(16_000, 0.1), ramp(16_000, 0.2)[::-1]

    async def main() -> None:
        adapter = Gated(record_then_done)
        adapter.release.set()
        async with scenario.serving(adapter):
            ws = await open_session(path, "ie115")
            for utterance in (first + b"\x01", second):
                await ws.send(utterance)
                await ws.send(finish_frame("ie115"))
                assert (await terminal(ws))["type"] == w.TRANSCRIPTION_COMPLETED
            await ws.close()
        assert [b"".join(heard) for heard in adapter.sessions[:2]] == [first, second]

    scenario.run(main)


def test_an_internal_session_with_a_degenerate_format_still_runs(scenario):
    """Framing must not take down a session the adapter would answer: a
    zero-channel format still reaches the adapter, which owns rejecting it."""
    path = scenario.path

    async def main() -> None:
        async with scenario.serving(FakeAdapter()):
            ws = await unix_connect(str(path), ping_interval=None)
            await ws.recv()
            config = session_config_to_wire(SessionConfig(audio_format=AudioFormat(channels=0)))
            start = {
                "type": "session.start",
                "protocol_version": PROTOCOL_VERSION,
                "config": config,
            }
            await ws.send(json.dumps(start))
            await ws.send(b"\x00\x00\x00")
            await ws.send(finish_frame("internal"))
            assert (await terminal(ws))["event"] == "transcription.done"

    scenario.run(main)


# --- the queue itself ------------------------------------------------------------

FMT = AudioFormat()


async def settle() -> None:
    for _ in range(5):
        await asyncio.sleep(0)


async def test_close_drains_what_was_accepted_then_ends():
    ingress = _Ingress(FMT)
    await ingress.put(b"ab")
    await ingress.put_boundary()
    await ingress.put(b"")  # nothing to queue
    await ingress.put(b"cd")
    ingress.close()
    got = [await ingress.get() for _ in range(4)]
    assert got == [PcmChunk(b"ab", FMT), _BOUNDARY, PcmChunk(b"cd", FMT), None]


async def test_close_wakes_a_waiting_consumer():
    ingress = _Ingress(FMT)
    getter = asyncio.create_task(ingress.get())
    await settle()
    ingress.close()
    assert await asyncio.wait_for(getter, BOUND) is None


async def test_a_full_queue_blocks_the_producer_until_the_consumer_takes_one():
    ingress = _Ingress(FMT, maxsize=2)
    await ingress.put(b"a0")
    await ingress.put(b"a1")
    putter = asyncio.create_task(ingress.put(b"a2"))
    await settle()
    assert not putter.done()
    assert await ingress.get() == PcmChunk(b"a0", FMT)
    await asyncio.wait_for(putter, BOUND)
    ingress.close()
    assert [await ingress.get() for _ in range(3)] == [
        PcmChunk(b"a1", FMT),
        PcmChunk(b"a2", FMT),
        None,
    ]


async def test_abort_releases_a_blocked_producer_and_discards_the_backlog():
    ingress = _Ingress(FMT, maxsize=1)
    await ingress.put(b"a0")
    putter = asyncio.create_task(ingress.put(b"a1"))
    await settle()
    ingress.abort()
    await asyncio.wait_for(putter, BOUND)
    await ingress.put(b"a2")  # after abort: discarded, never waits
    await ingress.put_boundary()
    assert await ingress.get() is None


async def test_abort_wakes_a_waiting_consumer():
    ingress = _Ingress(FMT)
    getter = asyncio.create_task(ingress.get())
    await settle()
    ingress.abort()
    assert await asyncio.wait_for(getter, BOUND) is None


async def test_a_cancelled_producer_queues_nothing_and_loses_no_wakeup():
    ingress = _Ingress(FMT, maxsize=1)
    await ingress.put(b"a0")
    cancelled = asyncio.create_task(ingress.put(b"xx"))
    waiting = asyncio.create_task(ingress.put(b"a1"))
    await settle()
    cancelled.cancel()
    await asyncio.wait({cancelled})
    assert await ingress.get() == PcmChunk(b"a0", FMT)
    await asyncio.wait_for(waiting, BOUND)
    ingress.close()
    assert [await ingress.get() for _ in range(2)] == [PcmChunk(b"a1", FMT), None]
