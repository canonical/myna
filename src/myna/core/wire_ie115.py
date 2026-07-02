"""IE115 wire dialect — codec between the flat ``myna.core`` vocab and the
OpenAI-Realtime-shaped subset the team settled on (``docs/IE115-resolution.md``,
frame contract in ``docs/architecture/ie115-wire.md``, T44/T45).

This is a *translation layer*, not a new protocol. The internal vocab
(``myna.core.events`` / ``session``) stays the semantic core; the transport
(``transport_ws``) picks a dialect by **shape-sniffing** the client's first
frame (``session.update`` = IE115, ``session.start`` = internal) and routes
encode/decode through here for IE115 connections.

What the mapping does (see the note for the full table + open questions):

- ``SessionConfig`` <-> nested ``session.audio.input.{format,transcription}``.
- ``transcription.progress{phase}`` <-> additive ``STATUS{state}`` liveness event
  (``preparing``->``loading``). We deliberately do **not** emit OpenAI's
  ``server_error/model_loading`` — loading is a liveness property, not an error.
- ``transcription.final`` <-> ``conversation.item.input_audio_transcription.completed``
  (one committed item per segment; we mint ``item_id``/``content_index`` since
  dictation has no conversation graph).
- ``transcription.done`` has **no** IE115 frame: the terminal is the connection
  close. The client decoder synthesises ``done`` on close from the accumulated
  ``completed`` transcripts (open question, note §7.4).
- ``transcription.error`` <-> ``error{error:{type,code,message}}``. The code
  mapping is **lossy** (our richer codes collapse onto IE115's four) — that
  lossiness is the T31 evidence, kept on purpose.

Audio: ``input_audio_buffer.append`` carries base64 PCM16 in JSON; the transport
also accepts raw WS binary frames (the frame-type hatch). Encoding/decoding of
the base64 payload lives here.
"""

from __future__ import annotations

import base64
import uuid

from myna.core.audio import AudioFormat, PcmChunk
from myna.core.events import (
    PHASE_PREPARING,
    PHASE_READY,
    PHASE_TRANSCRIBING,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionEvent,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.core.session import SessionConfig

# Additive liveness event (agreed 2026-07-01; name is provisional — note §7.5).
STATUS_EVENT = "status"

# IE115 frame type constants.
SESSION_UPDATE = "session.update"
SESSION_CREATED = "session.created"
SESSION_UPDATED = "session.updated"
INPUT_AUDIO_APPEND = "input_audio_buffer.append"
INPUT_AUDIO_COMMIT = "input_audio_buffer.commit"
TRANSCRIPTION_DELTA = "conversation.item.input_audio_transcription.delta"
TRANSCRIPTION_COMPLETED = "conversation.item.input_audio_transcription.completed"
ERROR = "error"

_PHASE_TO_STATE = {
    PHASE_PREPARING: "loading",
    PHASE_READY: "ready",
    PHASE_TRANSCRIBING: "transcribing",
}
_STATE_TO_PHASE = {v: k for k, v in _PHASE_TO_STATE.items()}

# Internal error code -> (IE115 error type, IE115 error code). Intentionally
# lossy: IE115 defines only four codes across two types and no
# terminal/recoverable axis. Collisions here are the T31 evidence.
_ERROR_TO_IE115 = {
    "unsupported_audio_format": ("invalid_request_error", "invalid_parameter"),
    "unsupported_protocol_version": ("invalid_request_error", "invalid_parameter"),
    "language_not_supported": ("invalid_request_error", "invalid_parameter"),
    "invalid_parameter": ("invalid_request_error", "invalid_parameter"),
    "unknown_parameter": ("invalid_request_error", "unknown_parameter"),
    "inference_failed": ("server_error", "server_error"),
    "adapter_crash": ("server_error", "server_error"),
    "internal": ("server_error", "server_error"),
}


# --- session config <-> nested IE115 `session` object ---------------------------


def session_config_to_ie115(config: SessionConfig, *, model: str | None = None) -> dict:
    """Flat ``SessionConfig`` -> the nested IE115 ``session`` object (the body of
    ``session.update`` / ``session.created`` / ``session.updated``)."""
    transcription: dict = {}
    if model is not None:
        transcription["model"] = model
    if config.language is not None:
        transcription["language"] = config.language
    if config.prompt is not None:
        transcription["prompt"] = config.prompt  # note §7.3: kept here, not top-level
    session: dict = {
        "type": "realtime",
        "audio": {
            "input": {
                "format": {"type": "audio/pcm", "rate": config.audio_format.sample_rate_hz},
                "transcription": transcription,
            }
        },
    }
    return session


def session_config_from_ie115(session: dict) -> SessionConfig:
    """Nested IE115 ``session`` object -> flat ``SessionConfig``. Channels/width
    are not expressible in IE115's ``format`` (note §7.2) so we assume our only
    accepted shape: mono 16-bit PCM."""
    audio_input = ((session.get("audio") or {}).get("input")) or {}
    fmt = audio_input.get("format") or {}
    transcription = audio_input.get("transcription") or {}
    rate = fmt.get("rate", AudioFormat().sample_rate_hz)
    return SessionConfig(
        audio_format=AudioFormat(sample_rate_hz=int(rate)),
        language=transcription.get("language"),
        prompt=transcription.get("prompt") or session.get("prompt"),
    )


# --- audio append <-> PcmChunk --------------------------------------------------


def append_to_pcm(frame: dict, fmt: AudioFormat) -> PcmChunk:
    """Decode an ``input_audio_buffer.append`` (base64 PCM16) into a PcmChunk."""
    data = base64.b64decode(frame.get("audio") or "")
    return PcmChunk(data=data, format=fmt)


def pcm_to_append(chunk: PcmChunk) -> dict:
    """Encode a PcmChunk as an ``input_audio_buffer.append`` frame (base64)."""
    return {"type": INPUT_AUDIO_APPEND, "audio": base64.b64encode(chunk.data).decode("ascii")}


# --- event encode (server side) -------------------------------------------------


class Ie115Encoder:
    """Encodes internal transcript events into IE115 server frames, holding the
    per-utterance ``item_id`` state IE115 requires (dictation has no
    conversation graph, so we mint one per committed segment)."""

    def __init__(self) -> None:
        self._item_id: str | None = None

    def _next_item_id(self) -> str:
        self._item_id = f"item_{uuid.uuid4().hex[:12]}"
        return self._item_id

    def encode(self, event: TranscriptionEvent) -> dict | None:
        """Return the IE115 frame for ``event``, or ``None`` if the event has no
        IE115 frame (``done`` — the terminal is the connection close)."""
        if isinstance(event, TranscriptionProgress):
            frame = {"type": STATUS_EVENT, "state": _PHASE_TO_STATE.get(event.phase, "transcribing")}
            if event.snippet:  # additive, optional (note §7.5)
                frame["snippet"] = event.snippet
            return frame
        if isinstance(event, TranscriptionFinal):
            return {
                "type": TRANSCRIPTION_COMPLETED,
                "item_id": self._next_item_id(),
                "content_index": 0,
                "transcript": event.text,
            }
        if isinstance(event, TranscriptionDone):
            return None  # terminal conveyed by close (note §7.4)
        if isinstance(event, TranscriptionError):
            etype, ecode = _ERROR_TO_IE115.get(event.code, ("server_error", "server_error"))
            return {"type": ERROR, "error": {"type": etype, "code": ecode, "message": event.message}}
        raise ValueError(f"cannot encode event: {event!r}")


# --- event decode (client side) -------------------------------------------------


class Ie115Decoder:
    """Decodes IE115 server frames into internal transcript events, accumulating
    ``completed`` transcripts so a terminal ``done`` can be synthesised when the
    connection closes (IE115 has no session-level terminal — note §7.4)."""

    def __init__(self) -> None:
        self._transcripts: list[str] = []
        self._terminated = False

    def decode(self, frame: dict) -> list[TranscriptionEvent]:
        """Zero or more internal events for one IE115 frame. Control frames
        (``session.created``/``session.updated``) yield nothing."""
        ftype = frame.get("type")
        if ftype in (SESSION_CREATED, SESSION_UPDATED):
            return []
        if ftype == STATUS_EVENT:
            phase = _STATE_TO_PHASE.get(frame.get("state"), PHASE_TRANSCRIBING)
            return [TranscriptionProgress(phase=phase, snippet=frame.get("snippet"))]
        if ftype == TRANSCRIPTION_DELTA:
            # IE115 delta is committed-incremental; we have no committed-delta
            # semantics until streaming (T08), so surface it as an unstable
            # liveness snippet, consistent with our progress.snippet (note §3).
            return [TranscriptionProgress(phase=PHASE_TRANSCRIBING, snippet=frame.get("delta"))]
        if ftype == TRANSCRIPTION_COMPLETED:
            text = frame.get("transcript", "")
            self._transcripts.append(text)
            return [TranscriptionFinal(text=text)]
        if ftype == ERROR:
            self._terminated = True
            err = frame.get("error") or {}
            return [TranscriptionError(code=err.get("code", "server_error"), message=err.get("message", ""))]
        return []  # unknown/ignored additive frame

    def on_close(self) -> list[TranscriptionEvent]:
        """Synthesise the terminal ``done`` from accumulated completes, unless a
        terminal ``error`` already went out. Idempotent."""
        if self._terminated:
            return []
        self._terminated = True
        return [TranscriptionDone(text=" ".join(t for t in self._transcripts if t))]
