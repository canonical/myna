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
- ``transcription.final`` <-> ``conversation.item.input_audio_transcription.delta``
  — committed, append-only segment text (the streaming revision contract,
  ``docs/architecture/streaming.md`` §3a). One ``item_id`` per utterance (we mint
  it; dictation has no conversation graph); every delta of the utterance and its
  ``completed`` share it.
- ``transcription.done`` <-> ``conversation.item.input_audio_transcription.completed``
  — one per ``input_audio_buffer.commit``, carrying the full utterance
  transcript. This is the utterance terminal: **the connection stays open**
  (OpenAI multi-commit shape, decided 2026-07-06); the *client* closes when it is
  finished. A close *before* ``completed`` therefore decodes as an error, never a
  synthesised ``done`` — a dead server must not read as a successful utterance.
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
    Disposition,
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
    "model_not_available": ("invalid_request_error", "invalid_parameter"),
    "invalid_parameter": ("invalid_request_error", "invalid_parameter"),
    "unknown_parameter": ("invalid_request_error", "unknown_parameter"),
    "inference_failed": ("server_error", "server_error"),
    "adapter_crash": ("server_error", "server_error"),
    "internal": ("server_error", "server_error"),
}


# --- session config <-> nested IE115 `session` object ---------------------------


def session_config_to_ie115(
    config: SessionConfig, *, model: str | None = None, streaming: bool | None = None
) -> dict:
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
    # Add streaming field if specified (T14, feature 007)
    if streaming is not None:
        session["streaming"] = streaming
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
    per-utterance ``item_id`` IE115 requires (dictation has no conversation
    graph, so we mint one per utterance): every ``delta`` of an utterance and
    its ``completed`` share the item; the ``completed`` retires it, so the next
    utterance on the same connection gets a fresh one."""

    def __init__(self) -> None:
        self._item_id: str | None = None

    def _item(self) -> str:
        if self._item_id is None:
            self._item_id = f"item_{uuid.uuid4().hex[:12]}"
        return self._item_id

    def encode(self, event: TranscriptionEvent) -> dict:
        """Return the IE115 frame for ``event``. Every event has a frame:
        ``done`` is the utterance's ``completed`` (the connection stays open)."""
        if isinstance(event, TranscriptionProgress):
            frame = {"type": STATUS_EVENT, "state": _PHASE_TO_STATE.get(event.phase, "transcribing")}
            if event.snippet:  # additive, optional (note §7.5)
                frame["snippet"] = event.snippet
            return frame
        if isinstance(event, TranscriptionFinal):
            frame = {
                "type": TRANSCRIPTION_DELTA,
                "item_id": self._item(),
                "content_index": 0,
                "delta": event.text,
                "disposition": event.disposition.value,  # Add disposition field (T11, feature 007)
            }
            # Add segment_index only for committed segments (T13, feature 007)
            if event.disposition == event.disposition.COMMITTED and event.segment_index is not None:
                frame["segment_index"] = event.segment_index
            return frame
        if isinstance(event, TranscriptionDone):
            item = self._item()
            self._item_id = None  # completed retires the utterance's item
            return {
                "type": TRANSCRIPTION_COMPLETED,
                "item_id": item,
                "content_index": 0,
                "transcript": event.text,
            }
        if isinstance(event, TranscriptionError):
            etype, ecode = _ERROR_TO_IE115.get(event.code, ("server_error", "server_error"))
            return {"type": ERROR, "error": {"type": etype, "code": ecode, "message": event.message}}
        raise ValueError(f"cannot encode event: {event!r}")


# --- event decode (client side) -------------------------------------------------


class Ie115Decoder:
    """Decodes IE115 server frames into internal transcript events for **one
    utterance**: the ``completed`` answering the client's commit is the terminal
    ``done``. The client closes the connection itself after it — never infer
    ``done`` from a server close."""

    def __init__(self) -> None:
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
            # Committed, append-only segment text — the IE115 face of our
            # `transcription.final` (streaming contract, streaming.md §3a).
            # Parse disposition field (T12, feature 007); default to committed for backward-compat
            disposition_str = frame.get("disposition", "committed")
            disposition = Disposition.COMMITTED if disposition_str == "committed" else Disposition.UNSTABLE
            segment_index = frame.get("segment_index")  # Optional, only present for committed
            return [TranscriptionFinal(
                text=frame.get("delta") or "",
                disposition=disposition,
                segment_index=segment_index,
            )]
        if ftype == TRANSCRIPTION_COMPLETED:
            self._terminated = True
            return [TranscriptionDone(text=frame.get("transcript", ""))]
        if ftype == ERROR:
            self._terminated = True
            err = frame.get("error") or {}
            return [TranscriptionError(code=err.get("code", "server_error"), message=err.get("message", ""))]
        return []  # unknown/ignored additive frame

    def on_close(self) -> list[TranscriptionEvent]:
        """A close *before* the utterance's terminal is a failure, not a result:
        the transcript may be truncated, so it must never surface as a clean
        ``done``. No-op (idempotent) after a real terminal."""
        if self._terminated:
            return []
        self._terminated = True
        return [
            TranscriptionError(
                code="connection_closed",
                message="connection closed before the utterance completed",
            )
        ]


# --- test helpers (T08-T010, feature 007) ----------------------------------------

def encode_delta(event: TranscriptionFinal, item_id: str) -> dict:
    """Helper for testing: encode a TranscriptionFinal as an IE115 delta frame."""
    encoder = Ie115Encoder()
    encoder._item_id = item_id  # Override the generated item_id for testing
    return encoder.encode(event)


def decode_delta(frame: dict) -> TranscriptionFinal:
    """Helper for testing: decode an IE115 delta frame to TranscriptionFinal."""
    decoder = Ie115Decoder()
    events = decoder.decode(frame)
    if not events or not isinstance(events[0], TranscriptionFinal):
        raise ValueError(f"Expected TranscriptionFinal, got {events}")
    return events[0]
