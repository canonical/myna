"""Transcript event vocabulary.

PROVISIONAL — this tracks the working vocabulary in CLAUDE.md, which simplifies
earlier IE114 drafts (no ``partial``/``replace``/epochs; retraction semantics
were dropped as confusing). If you add an event type, document it in CLAUDE.md
and flag it provisional.

Semantics:

- ``transcription.progress`` — "something is happening". May carry a short
  unstable snippet to animate a UI. Never display as committed text.
- ``transcription.final``    — stable, committed text for one utterance
  segment. Never retracted.
- ``transcription.done``     — end of session; carries the complete transcript.
- ``transcription.error``    — structured error; terminal for the session.

The wire encoding here is a transport-agnostic JSON object shape
(``{"event": <type>, "data": {...}}``), deliberately mirroring the SSE framing
in IE114 so the eventual transport (WebSocket per current direction) only has
to frame these objects, not reinvent them.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, ClassVar


@dataclass(frozen=True)
class Segment:
    """Timestamped transcript segment, relative to session start (seconds)."""

    start: float
    end: float
    text: str
    score: float | None = None


@dataclass(frozen=True)
class TranscriptionProgress:
    """Lightweight liveness signal. ``snippet`` is unstable text with no
    accuracy guarantee and no retraction semantics — UI animation only."""

    type: ClassVar[str] = "transcription.progress"
    snippet: str | None = None


@dataclass(frozen=True)
class TranscriptionFinal:
    """Stable, committed text for one utterance segment. Never retracted."""

    type: ClassVar[str] = "transcription.final"
    text: str = ""
    segments: tuple[Segment, ...] = ()


@dataclass(frozen=True)
class TranscriptionDone:
    """End of session. ``text`` is the complete transcript."""

    type: ClassVar[str] = "transcription.done"
    text: str = ""
    segments: tuple[Segment, ...] = ()


@dataclass(frozen=True)
class TranscriptionError:
    """Structured terminal error. ``code`` is a stable machine-readable
    identifier (e.g. ``language_not_supported``); ``message`` is for humans."""

    type: ClassVar[str] = "transcription.error"
    code: str = "internal"
    message: str = ""


TranscriptionEvent = (
    TranscriptionProgress | TranscriptionFinal | TranscriptionDone | TranscriptionError
)

_EVENT_TYPES: dict[str, type] = {
    cls.type: cls
    for cls in (TranscriptionProgress, TranscriptionFinal, TranscriptionDone, TranscriptionError)
}


def event_to_wire(event: TranscriptionEvent) -> dict[str, Any]:
    return {"event": event.type, "data": asdict(event)}


def event_from_wire(wire: dict[str, Any]) -> TranscriptionEvent:
    cls = _EVENT_TYPES.get(wire.get("event", ""))
    if cls is None:
        raise ValueError(f"unknown event type: {wire.get('event')!r}")
    data = dict(wire.get("data") or {})
    if "segments" in data:
        data["segments"] = tuple(Segment(**s) for s in data["segments"])
    return cls(**data)
