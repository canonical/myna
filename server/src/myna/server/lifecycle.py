"""Server-side model lifecycle: idle tracking and release policy (T27/T28).

``LifecycleService`` wraps any ``SttService`` to count active sessions and
stamp last-activity, so the server can release the model after an idle period.
``idle_monitor`` is the timer that drives it. Two release actions:

- ``"unload"`` (T27): drop the model weights in place, keep serving. Frees the
  bulk of memory; the process and (for CUDA) the context stay, so the next
  session re-warms faster than a full cold start.
- ``"exit"`` (T28): exit the process. Under socket activation systemd owns the
  socket and relaunches on the next connection — full process/VRAM release, at
  the cost of a full cold start on wake.

Which is better is backend-dependent (CTranslate2 wakes cheaply; NeMo/torch
pays a heavy import+CUDA-init tax), so it's a flag, not a hardcode.

``MemoryPressureMonitor`` (T10) is the runtime counterpart to
``dev/bench_guard.py``'s dev-time checks (T02): the same page-fault-thrash
signature that made the 2026-08-28 baseline's first pass wrong by 18x, caught
during real decoding instead of a benchmark. It lives here rather than in the
adapter because this is the closest existing neighbour to this concern - the
module already tracks busy/idle state and arena-trimming for the same
underlying reason (memory behaviour of a resident model). ``sample_majflt`` and
``MAJOR_PAGE_FAULT_THRESHOLD`` are the shared primitive: ``dev/bench_guard.py``
imports both from here instead of defining its own, so the dev-time guard and
the runtime detector can never drift on what "a major fault" means.
"""

from __future__ import annotations

import asyncio
import contextlib
import ctypes
import logging
import resource
import time
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path

from myna.core import EventSink, PcmChunk, SessionConfig


def _malloc_trim() -> bool:
    """Hand freed heap back to the OS. True if glibc's malloc_trim ran.

    Dropping the model frees the weights inside the process, but glibc parks
    them in its per-thread arenas rather than returning them: an idle parakeet
    sat at 1.08 GB RSS for hours after a successful unload, and a malloc_trim
    took it to 68 MB. Non-main arenas (one per ORT/BLAS worker thread, and the
    weights land in them) are only ever trimmed on request, so an unload that
    the user can see in `free -m` has to ask.
    """
    try:
        trim = ctypes.CDLL(None).malloc_trim
    except (OSError, AttributeError):  # not glibc (musl has no malloc_trim)
        return False
    trim.argtypes = (ctypes.c_size_t,)
    trim.restype = ctypes.c_int
    trim(0)
    return True


# --- Runtime memory-pressure detection (T10) --------------------------------
#
# A measurement pass once ran under an 800 MB cgroup cap against a 794 MB
# encoder (1.31 GB peak RSS) and was wrong by 18x with no OOM, no warning:
# just a working transcriber trading pages in and out on every decode. A real
# user on a memory-constrained machine hits the same thing and reports
# "dictation is slow" - nothing points at the actual cause. This section gives
# that user a plain-English notice instead of silence.

MAJOR_PAGE_FAULT_THRESHOLD = 1000
"""Per-decode major-fault budget. Healthy is order 10^2; a throttled machine
hit 202,792 major faults for a single 12 s decode. 1000 sits comfortably above
ordinary noise (a busy laptop, a cold page cache) and far below the failure
mode. Shared with dev/bench_guard.py's identical-purpose check so the
two thresholds cannot silently drift apart."""

PSI_SOME_AVG10_THRESHOLD = 5.0
"""Percent of the trailing 10 s a task on this cgroup stalled on memory
(``/proc/pressure/memory``, the ``some avg10=`` field). Idle machines read
~0.00; this is a secondary, optional corroborating signal, not a required one
- PSI can be unavailable under snap confinement or on cgroup v1, in which case
the major-fault delta alone must carry the signal (SPEC risk note)."""

# Peak RSS, unthrottled, measured 2026-08-28.
PEAK_RSS_BYTES = int(1.31 * 1024**3)
MEMORY_HEADROOM_BYTES = 512 * 1024**2
MIN_CGROUP_MEMORY_BYTES = PEAK_RSS_BYTES + MEMORY_HEADROOM_BYTES

MEMORY_PRESSURE_MESSAGE = (
    "This machine doesn't have enough free memory for the speech model, so "
    "transcription is running much slower than it should. Closing other "
    "applications, or using a smaller model, should fix it."
)


def sample_majflt() -> int:
    """Major page fault counter for the current process (aggregated across
    all its threads - ``RUSAGE_SELF``, not ``RUSAGE_THREAD``). Sample once
    before and once after a decode; the delta is the signal, not the absolute
    count, which includes faulting in the model itself at load time.

    The single shared primitive between this module and
    ``dev/bench_guard.py`` (T02): both need the identical number for the
    identical reason, and the dev script imports this function rather than
    defining its own so the two can't drift on what "a major fault" means.
    """
    return resource.getrusage(resource.RUSAGE_SELF).ru_majflt


def _read_psi_some_avg10(path: str = "/proc/pressure/memory") -> float | None:
    """The ``some avg10=`` field of PSI memory pressure, or ``None`` if the
    file is absent, unreadable, or unparsable - PSI is a Linux 4.20+ feature
    and can additionally be hidden under snap confinement."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        if not line.startswith("some "):
            continue
        for field in line.split():
            if field.startswith("avg10="):
                try:
                    return float(field.split("=", 1)[1])
                except ValueError:
                    return None
    return None


def _cgroup_memory_limit_bytes() -> int | None:
    """The smallest of ``memory.high``/``memory.max`` across this process's
    own cgroup v2 scope and every ancestor slice, or ``None`` if unreadable
    (cgroup v1, no cgroup, confinement) or nowhere limited below ``max``. A
    limit here is a static configuration fact, known at model-load time,
    before a single decode has run - an early prediction rather than a
    behavioural observation, which is why it's read once and cached rather
    than resampled per decode."""
    try:
        line = Path("/proc/self/cgroup").read_text(encoding="utf-8").strip().splitlines()[0]
    except (FileNotFoundError, IndexError, OSError):
        return None
    parts = line.split(":", 2)
    if len(parts) != 3:
        return None
    root = Path("/sys/fs/cgroup")
    scope = root / parts[2].lstrip("/")
    limits: list[int] = []
    p = scope
    while p != root and p != p.parent:
        for name in ("memory.high", "memory.max"):
            try:
                text = (p / name).read_text(encoding="utf-8").strip()
            except OSError:
                continue
            if text != "max":
                try:
                    limits.append(int(text))
                except ValueError:
                    pass
        p = p.parent
    return min(limits) if limits else None


@dataclass(frozen=True)
class _PressureEvidence:
    """What tripped the detector, for the log line - never shown to the user
    (SPEC: "Do not say 'page faults' to a user. Do log the numbers for a bug
    report.")."""

    majflt_delta: int
    psi_some_avg10: float | None
    cgroup_limit_bytes: int | None

    def __str__(self) -> str:
        bits = [f"majflt_delta={self.majflt_delta}"]
        if self.psi_some_avg10 is not None:
            bits.append(f"psi_some_avg10={self.psi_some_avg10}")
        if self.cgroup_limit_bytes is not None:
            bits.append(f"cgroup_limit_bytes={self.cgroup_limit_bytes}")
        return ", ".join(bits)


class MemoryPressureMonitor:
    """Detects page-fault thrashing around real decodes and reports it once
    per session, plainly, in the user's own words.

    Three pieces of evidence, any one of which is sufficient:

    1. A cgroup memory limit below what the model needs, read once at
       construction (model-load time) - a prediction available before any
       decode runs, cheap and exact (SPEC: "available at load time").
    2. The major-fault delta straddling one decode, over
       ``MAJOR_PAGE_FAULT_THRESHOLD`` - the direct behavioural signal, and
       the one guaranteed to work even where the other two are unavailable
       (snap confinement, cgroup v1).
    3. PSI ``some avg10`` over ``PSI_SOME_AVG10_THRESHOLD`` at the same
       moment - corroboration, optional.

    Debounced to one user-facing report per session: call ``begin_session()``
    when a session starts (mirrors ``LifecycleService``'s own re-arm on a
    fresh session) and ``observe_decode()`` around each decode. A machine
    that stays undersized warns again on the *next* session rather than
    training the user to distrust a single stale message.
    """

    def __init__(self, *, log: logging.Logger | None = None) -> None:
        self._log = log or logging.getLogger(__name__)
        self._warned = False
        self._cgroup_limit = _cgroup_memory_limit_bytes()
        self._cgroup_undersized = (
            self._cgroup_limit is not None and self._cgroup_limit < MIN_CGROUP_MEMORY_BYTES
        )
        if self._cgroup_undersized:
            self._log.warning(
                "memory pressure: cgroup memory limit %d bytes is below the %d bytes "
                "this model needs (peak RSS + headroom) - predicting decode thrash "
                "before any decode has run",
                self._cgroup_limit,
                MIN_CGROUP_MEMORY_BYTES,
            )

    def begin_session(self) -> None:
        """Re-arm the once-per-session debounce for a fresh session."""
        self._warned = False

    def observe_decode(self, majflt_before: int, majflt_after: int) -> str | None:
        """Call with ``sample_majflt()`` taken immediately before and after
        one decode. Returns the user-facing message the first time this
        session crosses the threshold, else ``None`` - including on every
        subsequent call this session, even if the condition persists."""
        if self._warned:
            return None
        delta = majflt_after - majflt_before
        psi = _read_psi_some_avg10()
        thrashing = (
            delta > MAJOR_PAGE_FAULT_THRESHOLD
            or (psi is not None and psi > PSI_SOME_AVG10_THRESHOLD)
            or self._cgroup_undersized
        )
        if not thrashing:
            return None
        self._warned = True
        evidence = _PressureEvidence(delta, psi, self._cgroup_limit)
        self._log.warning("memory pressure detected during decode: %s", evidence)
        return MEMORY_PRESSURE_MESSAGE


class LifecycleService:
    """Wraps an ``SttService``, tracking activity for idle release."""

    def __init__(self, service) -> None:
        self._service = service
        self._active = 0
        self._last = time.monotonic()
        self._released = False

    @property
    def candidate(self):
        return self._service.candidate

    @property
    def streaming(self) -> bool | None:
        """Delegate streaming mode to the wrapped adapter (T027)."""
        return getattr(self._service, "streaming", None)

    def capabilities(self):
        return self._service.capabilities()

    async def run_session(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        self._active += 1
        self._released = False  # re-arm: a fresh session means a loaded model
        try:
            await self._service.run_session(config, audio, emit)
        finally:
            self._active -= 1
            self._last = time.monotonic()

    @property
    def busy(self) -> bool:
        return self._active > 0

    def idle_seconds(self) -> float:
        return 0.0 if self.busy else time.monotonic() - self._last

    async def unload(self) -> None:
        unload = getattr(self._service, "unload", None)
        if unload is not None:
            await unload()
        # Off-loop: trim walks every arena holding the malloc lock.
        await asyncio.to_thread(_malloc_trim)

    async def maybe_release(
        self, action: str, stop: asyncio.Event, log: logging.Logger | None = None
    ) -> None:
        """Release once per idle period (no-op if busy or already released)."""
        if self.busy or self._released:
            return
        self._released = True
        if action == "exit":
            if log:
                log.info("idle -> exiting for socket reactivation")
            stop.set()
        else:
            if log:
                log.info("idle -> unloading model")
            await self.unload()


async def idle_monitor(
    lifecycle: LifecycleService,
    timeout: float,
    action: str,
    stop: asyncio.Event,
    *,
    log: logging.Logger | None = None,
) -> None:
    """Release the model once ``lifecycle`` has been idle for ``timeout`` s.
    Returns when ``stop`` is set."""
    tick = max(0.5, min(timeout, 5.0))
    while not stop.is_set():
        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(stop.wait(), timeout=tick)
        if stop.is_set():
            return
        if not lifecycle.busy and lifecycle.idle_seconds() >= timeout:
            await lifecycle.maybe_release(action, stop, log)
