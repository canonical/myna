"""Transcript event vocabulary.

PROVISIONAL — this tracks the working vocabulary in CLAUDE.md, which simplifies
earlier IE114 drafts (no ``partial``/``replace``/epochs; retraction semantics
were dropped as confusing). If you add an event type, document it in CLAUDE.md,
flag it provisional, and bump ``myna.core.protocol.PROTOCOL_VERSION`` — the
event vocabulary is versioned as part of the wire contract (``event_from_wire``
rejects unknown types, so a new event is a breaking change for old clients).

IE115 reconciliation (T36, see ``docs/IE115-deviations.md`` §1.1): we keep these
flat ``transcription.*`` names rather than adopting IE115's five-deep OpenAI
``conversation.item.input_audio_transcription.{delta,completed,failed}`` — there
is no conversation in dictation, only a stream becoming text. Mapping for anyone
porting an IE115/OpenAI client: ``…transcription.delta`` →
``transcription.progress`` (its ``snippet`` is unstable liveness, *not* IE115's
committed incremental delta — we emit no committed deltas until streaming, T08);
``…transcription.completed`` → ``transcription.final`` (per segment) +
``transcription.done`` (session end); ``…transcription.failed`` →
``transcription.error``. The single ``error`` channel replaces IE115's split
top-level-``error`` / per-item-``failed``.

Semantics:

- ``transcription.progress`` — "something is happening". May carry a short
  unstable snippet to animate a UI. Never display as committed text. The
  ``phase`` field distinguishes *what* is happening: ``"preparing"`` (the
  model is loading — show "loading model…", expect a wait) vs ``"transcribing"``
  (audio is being processed). This is the lifecycle signal from plan T26,
  deliberately a field on the liveness we already emit rather than a new
  ``session.starting`` event — and a single phase, not a full state machine
  (the audio-push model makes "listening" the client's own business).
- ``transcription.final``    — stable, committed text for one utterance
  segment. Never retracted.
- ``transcription.done``     — end of session; carries the complete transcript.
- ``transcription.error``    — structured error; terminal for the session.

The wire encoding here is a transport-agnostic JSON object shape
(``{"event": <type>, "data": {...}}``), so a transport only has to frame these
objects, not reinvent them. Over ws+unix these go out as JSON text frames; the
``"event"`` key distinguishes them from control frames (``session.created``),
which carry a ``"type"`` key.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, ClassVar


# progress.phase values
PHASE_PREPARING = "preparing"  # model loading; client should show "loading…"
PHASE_TRANSCRIBING = "transcribing"  # audio being processed


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
    accuracy guarantee and no retraction semantics — UI animation only.
    ``phase`` is ``"preparing"`` while the model loads, else ``"transcribing"``
    (the default), so a cold load reads as "loading…" not a hang."""

    type: ClassVar[str] = "transcription.progress"
    snippet: str | None = None
    phase: str = PHASE_TRANSCRIBING


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
