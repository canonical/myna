"""Idle-unload / socket-activation lifecycle (T27/T28). No model or sockets."""

import asyncio
import socket

from myna.core import SessionConfig, WsUnixClient, serve_unix, systemd_socket
from myna.server.lifecycle import LifecycleService, idle_monitor
from myna.testbed import FakeAdapter, Harness, SilenceSource
from myna.testbed.adapter import Candidate


class StubService:
    def __init__(self):
        self.unloaded = 0
        self.ran = 0

    @property
    def candidate(self):
        return Candidate("m", "e", "s")

    async def run_session(self, config, audio, emit):
        self.ran += 1
        async for _ in audio:
            pass

    async def unload(self):
        self.unloaded += 1


async def _empty_audio():
    for _ in ():
        yield


async def _noop_emit(event):
    pass


async def _run_one(life):
    await life.run_session(SessionConfig(), _empty_audio(), _noop_emit)


async def test_run_session_delegates_and_tracks_idle():
    stub = StubService()
    life = LifecycleService(stub)
    assert not life.busy
    await _run_one(life)
    assert stub.ran == 1
    assert not life.busy
    assert life.idle_seconds() >= 0.0


async def test_unload_action_releases_once_then_rearms():
    stub = StubService()
    life = LifecycleService(stub)
    stop = asyncio.Event()

    await life.maybe_release("unload", stop)
    await life.maybe_release("unload", stop)  # idempotent within one idle period
    assert stub.unloaded == 1
    assert not stop.is_set()

    await _run_one(life)  # a fresh session re-arms
    await life.maybe_release("unload", stop)
    assert stub.unloaded == 2


async def test_exit_action_sets_stop_does_not_unload():
    stub = StubService()
    life = LifecycleService(stub)
    stop = asyncio.Event()
    await life.maybe_release("exit", stop)
    assert stop.is_set()
    assert stub.unloaded == 0


async def test_busy_session_blocks_release():
    gate = asyncio.Event()

    class Blocking(StubService):
        async def run_session(self, config, audio, emit):
            await gate.wait()

    life = LifecycleService(Blocking())
    stop = asyncio.Event()
    task = asyncio.ensure_future(_run_one(life))
    await asyncio.sleep(0)  # let the session start
    assert life.busy
    await life.maybe_release("unload", stop)
    assert not stop.is_set()  # nothing released while a session is in flight
    gate.set()
    await task
    assert not life.busy


async def test_idle_monitor_releases_after_timeout():
    stub = StubService()
    life = LifecycleService(stub)
    stop = asyncio.Event()
    monitor = asyncio.ensure_future(idle_monitor(life, 0.05, "unload", stop))
    await asyncio.sleep(0.7)  # > the 0.5s minimum check tick
    assert stub.unloaded >= 1
    stop.set()
    await monitor


def test_systemd_socket_none_without_activation(monkeypatch):
    monkeypatch.delenv("LISTEN_PID", raising=False)
    monkeypatch.delenv("LISTEN_FDS", raising=False)
    assert systemd_socket() is None


def test_systemd_socket_none_for_other_pid(monkeypatch):
    monkeypatch.setenv("LISTEN_PID", "999999")  # not us
    monkeypatch.setenv("LISTEN_FDS", "1")
    assert systemd_socket() is None


def test_set_proc_name_sets_and_truncates_comm():
    from myna.server.cli import _set_proc_name

    original = open("/proc/self/comm").read().strip()
    try:
        _set_proc_name("myna-nemotron-very-long")
        assert open("/proc/self/comm").read().strip() == "myna-nemotron-v"  # 15 chars
    finally:
        _set_proc_name(original)


async def test_serves_on_a_pre_bound_socket(tmp_path):
    # the T28 mechanism: systemd hands us a bound+listening socket; we serve
    # on it instead of binding a path.
    path = tmp_path / "a.sock"
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(str(path))
    sock.listen()
    async with serve_unix(FakeAdapter(), sock=sock):
        record = await Harness().run(
            client=WsUnixClient(path),
            candidate=FakeAdapter().candidate,
            source=SilenceSource(0.3),
        )
    assert record.events[-1].event.type == "transcription.done"
