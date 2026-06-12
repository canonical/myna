"""Text output abstraction (UD129 Text Injection Layer).

The MVP user-facing model is commit-only: provisional text is never shown in
the target application; an activity indicator covers the gap. IBus is the
first backend, but it is an implementation detail behind this protocol — a
Wayland input-method backend must be addable without touching callers.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable


@runtime_checkable
class TextInjector(Protocol):
    """One injection session against the text surface focused at session
    start. The target must not be retargeted mid-session (UD129: no silent
    commits into a different application after focus changes)."""

    async def start(self) -> None:
        """Acquire the currently focused target. Must fail (raise) rather
        than bind to a secure input surface (password field, lock screen,
        polkit prompt) where detectable."""
        ...

    async def set_activity(self, active: bool) -> None:
        """Show or hide the recording/transcription activity indicator."""
        ...

    async def commit(self, text: str) -> None:
        """Insert committed, stable text into the target. Committed text is
        never modified afterwards."""
        ...

    async def cancel(self) -> None:
        """Abort without injecting anything further. Idempotent."""
        ...

    async def end(self) -> None:
        """Finalize and release the target. Idempotent."""
        ...
