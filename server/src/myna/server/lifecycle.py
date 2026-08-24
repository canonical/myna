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
"""

from __future__ import annotations

import asyncio
import contextlib
import ctypes
import logging
import time
from collections.abc import AsyncIterator

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
