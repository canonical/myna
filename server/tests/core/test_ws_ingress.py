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
import itertools
import json
import logging
import socket
import threading
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
from pathlib import Path
from typing import Any

import pytest
from websockets.asyncio.client import ClientConnection, unix_connect
from websockets.asyncio.server import Server
from websockets.exceptions import ConnectionClosed

from myna.core import (
    AudioFormat,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    serve_unix,
    transport_ws,
)
from myna.core import wire_ie115 as w
from myna.core.protocol import PROTOCOL_VERSION
from myna.core.session import session_config_to_wire
from myna.core.transport_ws import _BOUNDARY, _Ingress
from myna.testbed import FakeAdapter

BOUND = 5.0
MIB = 1 << 20
SECOND = bytes(range(256)) * 125  # one second of 16 kHz PCM
# Many times what the server buffers plus what the kernel absorbs, so the
# sender stalls unless the server keeps reading; see open_session.
FLOOD = 512
DIALECTS = ("internal", "ie115")


class Scenario:
    """Runs one scenario on its own loop and thread. An assertion failing
    inside ``serving`` is recorded before the server's teardown, which may be
    the very thing that hangs."""

    def __init__(self, tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
        self.path = tmp_path / "myna.sock"
        self._caplog = caplog
        self.errors: list[BaseException] = []
        self._settled = threading.Event()

    @contextlib.asynccontextmanager
    async def serving(self, adapter: Any) -> AsyncIterator[Server]:
        async with serve_unix(adapter, self.path) as server:
            try:
                yield server
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
        failures = [r.getMessage() for r in self._caplog.records if r.levelno >= logging.ERROR]
        assert failures == []


@pytest.fixture
def scenario(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> Scenario:
    return Scenario(tmp_path, caplog)


@pytest.fixture
def ingresses(monkeypatch: pytest.MonkeyPatch) -> list[_Ingress]:
    """Every ingress queue the server opens, to inspect its occupancy."""
    opened: list[_Ingress] = []

    class Recorded(_Ingress):
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            super().__init__(*args, **kwargs)
            opened.append(self)

    monkeypatch.setattr(transport_ws, "_Ingress", Recorded)
    return opened


async def open_session(path: Path, dialect: str, rate: int = 16_000) -> ClientConnection:
    # A small send buffer keeps the kernel from absorbing megabytes of audio
    # the server has not read, so a stalled reader shows as a stalled sender.
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
    sock.connect(str(path))
    # No compression: repetitive test audio would deflate to nothing.
    ws = await unix_connect(sock=sock, ping_interval=None, compression=None)
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
    (failure,) = [r for r in caplog.records if r.getMessage() == "ingress reader failed"]
    assert failure.exc_info is not None


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
    zero-channel format still reaches the adapter, which owns rejecting it,
    and its bytes arrive unframed."""
    path = scenario.path

    async def main() -> None:
        adapter = Gated(record_then_done)
        adapter.release.set()
        async with scenario.serving(adapter):
            ws = await unix_connect(str(path), ping_interval=None)
            await ws.recv()
            config = session_config_to_wire(SessionConfig(audio_format=AudioFormat(channels=0)))
            start = {
                "type": "session.start",
                "protocol_version": PROTOCOL_VERSION,
                "config": config,
            }
            await ws.send(json.dumps(start))
            await ws.send(b"\x01\x02\x03")
            await ws.send(finish_frame("internal"))
            assert (await terminal(ws))["event"] == "transcription.done"
        assert adapter.sessions == [[b"\x01\x02\x03"]]

    scenario.run(main)


@pytest.mark.parametrize("dialect", DIALECTS)
def test_a_graceful_finish_after_backpressure_delivers_every_byte_in_order(
    dialect, scenario, ingresses
):
    """A slow adapter backpressures the client instead of growing the queue
    past its byte budget, and nothing accepted is lost or reordered."""

    def frames() -> Iterable[bytes]:
        return (bytes([i % 251]) * 32_000 for i in range(FLOOD))

    path = scenario.path

    async def main() -> None:
        adapter = Gated(record_then_done)
        async with scenario.serving(adapter):
            ws = await open_session(path, dialect)
            frames_then_finish = itertools.chain(frames(), [finish_frame(dialect)])
            feed = asyncio.create_task(send_all(ws, frames_then_finish))
            assert await stalls(feed), "the queue never filled"
            (ingress,) = ingresses
            assert MIB - 32_000 < ingress._bytes <= MIB
            adapter.release.set()
            assert (await terminal(ws)).get("type") != w.ERROR
            await asyncio.wait_for(feed, BOUND)
            await ws.close()
        assert b"".join(adapter.sessions[0]) == b"".join(frames())

    scenario.run(main)


def test_the_server_buffers_at_most_one_unread_websocket_frame(scenario):
    """websockets' own receive buffer sits in front of the ingress budget: at
    its default of 16 frames of up to 1 MiB each it would dwarf the queue."""
    path = scenario.path

    async def main() -> None:
        async with scenario.serving(FakeAdapter()) as server:
            ws = await open_session(path, "internal")
            (connection,) = server.connections
            assert connection.recv_messages.high == 1
            await ws.close()

    scenario.run(main)


def test_an_oversized_normalized_append_is_split_to_the_budget(scenario, monkeypatch, ingresses):
    """8 kHz upsampled to 16 kHz doubles an append past the budget: it is
    split into whole-sample pieces that fit, rather than waiting forever for
    room it can never have, and the pieces reach the adapter in order."""
    from myna.core.resample import Resampler

    capacity = 64_000
    monkeypatch.setattr(transport_ws, "_INGRESS_CAPACITY_BYTES", capacity)
    path = scenario.path
    pcm = ramp(8_000, 3.0)  # 48 kB here, 96 kB at the adapter's rate
    whole = Resampler(8_000, 16_000)
    expected = whole.feed(pcm) + whole.flush()

    async def main() -> None:
        adapter = Gated(record_then_done)
        async with scenario.serving(adapter):
            ws = await open_session(path, "ie115", 8_000)
            await ws.send(pcm)
            (ingress,) = ingresses

            async def blocked_on_the_second_piece() -> None:
                while not ingress._putters:
                    await asyncio.sleep(0.01)

            await asyncio.wait_for(blocked_on_the_second_piece(), BOUND)
            assert ingress._bytes == capacity
            adapter.release.set()
            await ws.send(finish_frame("ie115"))
            assert (await terminal(ws))["type"] == w.TRANSCRIPTION_COMPLETED
            await ws.close()
        heard = adapter.sessions[0]
        assert max(len(chunk) for chunk in heard) == capacity
        assert b"".join(heard) == expected

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
    ingress = _Ingress(FMT, capacity_bytes=4)
    await ingress.put(b"a0")
    await ingress.put(b"a1")
    putter = asyncio.create_task(ingress.put(b"a2"))
    await settle()
    assert not putter.done()
    assert await ingress.get() == PcmChunk(b"a0a1", FMT)
    await asyncio.wait_for(putter, BOUND)
    ingress.close()
    assert [await ingress.get() for _ in range(2)] == [PcmChunk(b"a2", FMT), None]


async def test_abort_releases_a_blocked_producer_and_discards_the_backlog():
    ingress = _Ingress(FMT, capacity_bytes=2)
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
    ingress = _Ingress(FMT, capacity_bytes=2)
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


async def fill(ingress: _Ingress, pieces: Iterable[bytes]) -> int:
    """Put pieces until a put blocks; how many went in without waiting."""
    for count, piece in enumerate(pieces):
        putter = asyncio.create_task(ingress.put(piece))
        await settle()
        if not putter.done():
            putter.cancel()
            return count
    raise AssertionError("never blocked")


async def test_normal_appends_fill_one_mebibyte_of_pcm():
    ingress = _Ingress(FMT)
    accepted = await fill(ingress, itertools.repeat(bytes(3200)))
    assert accepted == MIB // 3200
    assert ingress._bytes == accepted * 3200


async def test_an_append_larger_than_the_budget_is_split_into_pieces_that_fit():
    ingress = _Ingress(FMT)
    pcm = bytes(range(256)) * (MIB * 5 // 2 // 256)
    putter = asyncio.create_task(ingress.put(pcm))
    await settle()
    assert not putter.done()
    assert ingress._bytes == MIB
    pieces = []
    while (chunk := await ingress.get()) is not None:
        pieces.append(chunk.data)
        if putter.done():
            ingress.close()
    assert [len(piece) for piece in pieces] == [MIB, MIB, MIB // 2]
    assert b"".join(pieces) == pcm


async def test_pieces_of_a_split_append_are_whole_frames():
    ingress = _Ingress(AudioFormat(channels=2), capacity_bytes=10)
    putter = asyncio.create_task(ingress.put(bytes(20)))
    sizes = []
    while len(sizes) < 3:
        sizes.append(len((await ingress.get()).data))
    await asyncio.wait_for(putter, BOUND)
    assert sizes == [8, 8, 4]


def test_a_capacity_must_hold_a_whole_frame():
    with pytest.raises(ValueError, match="3 bytes hold no 4-byte frame"):
        _Ingress(AudioFormat(channels=2), capacity_bytes=3)


async def test_one_byte_appends_are_bounded_in_bytes_and_in_items():
    """A client sending a byte at a time must not turn the byte budget into
    hundreds of thousands of queued objects: tiny chunks coalesce."""
    from myna.core.audio import PcmFramer

    capacity = 64_000
    ingress = _Ingress(FMT, capacity_bytes=capacity)
    framer = PcmFramer(2)
    for _ in range(capacity):
        await ingress.put(framer.feed(b"\x01"))
    assert ingress._bytes == capacity
    assert len(ingress._items) == capacity // 3200
    assert await fill(ingress, itertools.repeat(b"\x01\x01")) == 0


async def test_coalescing_keeps_order_and_never_crosses_a_boundary():
    ingress = _Ingress(FMT)
    for piece in (b"a0", b"a1", b"a2"):
        await ingress.put(piece)
    await ingress.put_boundary()
    await ingress.put(b"b0")
    await ingress.put(bytes(3198))
    await ingress.put(b"c0")  # the tail is full: a new chunk
    ingress.close()
    got = []
    while (item := await ingress.get()) is not None:
        got.append(item if item is _BOUNDARY else item.data)
    assert got == [b"a0a1a2", _BOUNDARY, b"b0" + bytes(3198), b"c0"]


async def test_a_second_commit_waits_until_the_first_is_consumed():
    """Commits carry no audio, so the byte budget alone would let a commit
    flood queue without bound."""
    ingress = _Ingress(FMT)
    await ingress.put_boundary()
    second = asyncio.create_task(ingress.put_boundary())
    await settle()
    assert not second.done()
    assert await ingress.get() is _BOUNDARY
    await asyncio.wait_for(second, BOUND)
