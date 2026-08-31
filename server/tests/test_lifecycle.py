"""Idle-unload / socket-activation lifecycle (T27/T28), plus runtime
memory-pressure detection (T10). No model or sockets."""

import asyncio
import platform
import socket
from pathlib import Path
from unittest import mock

import pytest

from myna.core import SessionConfig, WsUnixClient, serve_unix, systemd_socket
from myna.server import lifecycle
from myna.server.lifecycle import (
    MAJOR_PAGE_FAULT_THRESHOLD,
    PSI_SOME_AVG10_THRESHOLD,
    LifecycleService,
    MemoryPressureMonitor,
    idle_monitor,
    sample_majflt,
)
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


async def test_unload_trims_the_heap_after_the_adapter_releases():
    """Freed weights sit in glibc's arenas until trimmed, so the trim must
    happen, and only once the adapter has actually dropped them."""
    stub = StubService()
    life = LifecycleService(stub)
    seen = []
    with mock.patch.object(lifecycle, "_malloc_trim", lambda: seen.append(stub.unloaded)):
        await life.unload()
    assert seen == [1]


@pytest.mark.skipif(platform.libc_ver()[0] != "glibc", reason="malloc_trim is glibc-only")
async def test_malloc_trim_runs_on_glibc():
    """Guards the ctypes lookup: a rename or a bad symbol would fall into the
    'not glibc' branch and silently stop returning memory."""
    assert lifecycle._malloc_trim() is True


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


# --- T10: runtime memory-pressure detection --------------------------------


def test_sample_majflt_is_a_nonnegative_int():
    # Smoke test on the real process; the delta (not the absolute value) is
    # the signal everywhere else, but the primitive itself must not explode.
    assert sample_majflt() >= 0


def test_monitor_healthy_machine_stays_silent(monkeypatch):
    """No cgroup limit, no PSI signal, majflt delta well under threshold:
    the acceptance criterion's 'lifted' half -- no false positive."""
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: None)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)
    monitor = MemoryPressureMonitor()
    monitor.begin_session()
    assert monitor.observe_decode(0, 5) is None


def test_monitor_undersized_cgroup_warns_on_first_decode_even_with_no_faults(monkeypatch):
    """Reproduces the 2026-08-28 discovery deterministically: a cgroup limit
    below what the model needs is known at model-load time, before a single
    decode has faulted -- this is the 'memory.high = 800 MB' half of the
    acceptance criterion, made safe to test without inducing real thrash."""
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: 800 * 1024**2)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)
    monitor = MemoryPressureMonitor()
    monitor.begin_session()
    warning = monitor.observe_decode(0, 0)  # zero faults -- cgroup fact alone is enough
    assert warning == lifecycle.MEMORY_PRESSURE_MESSAGE
    assert "page fault" not in warning.lower()  # SPEC: never say "page faults" to a user


def test_monitor_majflt_delta_over_threshold_warns(monkeypatch):
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: None)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: None)
    monitor = MemoryPressureMonitor()
    monitor.begin_session()
    assert monitor.observe_decode(0, MAJOR_PAGE_FAULT_THRESHOLD) is None  # exactly at, not over
    assert monitor.observe_decode(0, MAJOR_PAGE_FAULT_THRESHOLD + 1) is not None


def test_monitor_psi_over_threshold_warns_even_with_no_faults(monkeypatch):
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: None)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: PSI_SOME_AVG10_THRESHOLD + 1.0)
    monitor = MemoryPressureMonitor()
    monitor.begin_session()
    assert monitor.observe_decode(0, 0) is not None


def test_monitor_debounces_within_a_session_then_rearms_on_the_next(monkeypatch):
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: 800 * 1024**2)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: 0.0)
    monitor = MemoryPressureMonitor()

    monitor.begin_session()
    assert monitor.observe_decode(0, 0) is not None  # first decode of session 1: warns
    assert monitor.observe_decode(0, 0) is None  # second decode, same session: debounced
    assert monitor.observe_decode(0, 999999) is None  # even a huge delta: still debounced

    monitor.begin_session()  # a fresh session re-arms (mirrors LifecycleService)
    assert monitor.observe_decode(0, 0) is not None


def test_monitor_healthy_cgroup_does_not_mask_a_real_page_fault_storm(monkeypatch):
    """A generously-sized cgroup must not suppress the behavioural signal --
    the cgroup check is one *additional* way to trigger, not a gate on the
    others."""
    monkeypatch.setattr(lifecycle, "_cgroup_memory_limit_bytes", lambda: 8 * 1024**3)
    monkeypatch.setattr(lifecycle, "_read_psi_some_avg10", lambda: None)
    monitor = MemoryPressureMonitor()
    monitor.begin_session()
    assert monitor.observe_decode(0, MAJOR_PAGE_FAULT_THRESHOLD + 1) is not None


def test_read_psi_degrades_to_none_when_file_is_absent():
    assert lifecycle._read_psi_some_avg10("/nonexistent/path/for/sure") is None


def test_read_psi_degrades_to_none_on_malformed_content(tmp_path):
    bogus = tmp_path / "memory"
    bogus.write_text("garbage\nnot the psi format at all\n")
    assert lifecycle._read_psi_some_avg10(str(bogus)) is None


def test_read_psi_parses_the_real_kernel_format(tmp_path):
    psi = tmp_path / "memory"
    psi.write_text(
        "some avg10=12.34 avg60=5.00 avg300=1.00 total=999\n"
        "full avg10=1.00 avg60=0.50 avg300=0.10 total=111\n"
    )
    assert lifecycle._read_psi_some_avg10(str(psi)) == 12.34


def test_cgroup_memory_limit_degrades_to_none_when_unreadable(monkeypatch):
    """Snap confinement, cgroup v1, or no cgroup at all -- must not raise."""

    def _raise(self, *a, **kw):
        raise OSError("confined")

    monkeypatch.setattr(Path, "read_text", _raise)
    assert lifecycle._cgroup_memory_limit_bytes() is None


def test_cgroup_memory_limit_walks_ancestors_and_takes_the_minimum(tmp_path, monkeypatch):
    """The limit that bit the 2026-08-28 baseline was on the process's own
    scope, but a limit on any ancestor slice has the same effect -- mirrors
    dev/parakeet/bench_guard.py's identical-purpose walk (T02)."""
    (tmp_path / "proc_self_cgroup").write_text("0::/leaf/child\n")
    cgroup_root = tmp_path / "sys_fs_cgroup"
    leaf, child = cgroup_root / "leaf", cgroup_root / "leaf" / "child"
    child.mkdir(parents=True)
    (leaf / "memory.high").write_text("max\n")
    (leaf / "memory.max").write_text(f"{3 * 1024**3}\n")
    (child / "memory.high").write_text(f"{1024**3}\n")  # smallest: the answer
    (child / "memory.max").write_text("max\n")

    real_path = Path

    def fake_path(p, *args, **kwargs):
        if p == "/proc/self/cgroup":
            return real_path(tmp_path / "proc_self_cgroup")
        if p == "/sys/fs/cgroup":
            return real_path(cgroup_root)
        return real_path(p, *args, **kwargs)

    monkeypatch.setattr(lifecycle, "Path", fake_path)
    assert lifecycle._cgroup_memory_limit_bytes() == 1024**3


def test_cgroup_memory_limit_none_when_nothing_is_limited(tmp_path, monkeypatch):
    (tmp_path / "proc_self_cgroup").write_text("0::/leaf\n")
    cgroup_root = tmp_path / "sys_fs_cgroup"
    leaf = cgroup_root / "leaf"
    leaf.mkdir(parents=True)
    (leaf / "memory.high").write_text("max\n")
    (leaf / "memory.max").write_text("max\n")

    real_path = Path

    def fake_path(p, *args, **kwargs):
        if p == "/proc/self/cgroup":
            return real_path(tmp_path / "proc_self_cgroup")
        if p == "/sys/fs/cgroup":
            return real_path(cgroup_root)
        return real_path(p, *args, **kwargs)

    monkeypatch.setattr(lifecycle, "Path", fake_path)
    assert lifecycle._cgroup_memory_limit_bytes() is None
