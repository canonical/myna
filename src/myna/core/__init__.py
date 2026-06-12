"""Shared vocabulary for all Myna components.

Nothing in here may import from ``myna.testbed`` or ``myna.desktop``.
"""

from myna.core.audio import AudioFormat, AudioSource, PcmChunk
from myna.core.events import (
    PHASE_PREPARING,
    PHASE_TRANSCRIBING,
    Segment,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionEvent,
    TranscriptionFinal,
    TranscriptionProgress,
    event_from_wire,
    event_to_wire,
)
from myna.core.session import SessionConfig, session_config_from_wire, session_config_to_wire
from myna.core.transport import EventSink, LoopbackClient, SttClient, SttService, SttSession
from myna.core.transport_ws import WsUnixClient, serve_unix

__all__ = [
    "PHASE_PREPARING",
    "PHASE_TRANSCRIBING",
    "AudioFormat",
    "AudioSource",
    "EventSink",
    "LoopbackClient",
    "PcmChunk",
    "Segment",
    "SessionConfig",
    "SttClient",
    "SttService",
    "SttSession",
    "TranscriptionDone",
    "TranscriptionError",
    "TranscriptionEvent",
    "TranscriptionFinal",
    "TranscriptionProgress",
    "WsUnixClient",
    "event_from_wire",
    "event_to_wire",
    "serve_unix",
    "session_config_from_wire",
    "session_config_to_wire",
]
