"""Shared vocabulary for all Myna components.

Nothing in here may import from ``myna.testbed`` or ``myna.desktop``.
"""

from myna.core.audio import AudioFormat, AudioSource, PcmChunk
from myna.core.capabilities import (
    Capabilities,
    capabilities_from_wire,
    capabilities_to_wire,
)
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
from myna.core.protocol import (
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
    is_supported,
)
from myna.core.session import SessionConfig, session_config_from_wire, session_config_to_wire
from myna.core.transport import EventSink, LoopbackClient, SttClient, SttService, SttSession
from myna.core.transport_ws import WsUnixClient, serve_unix, systemd_socket

__all__ = [
    "PHASE_PREPARING",
    "PHASE_TRANSCRIBING",
    "PROTOCOL_VERSION",
    "SUPPORTED_PROTOCOL_VERSIONS",
    "AudioFormat",
    "AudioSource",
    "Capabilities",
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
    "capabilities_from_wire",
    "capabilities_to_wire",
    "event_from_wire",
    "event_to_wire",
    "is_supported",
    "serve_unix",
    "systemd_socket",
    "session_config_from_wire",
    "session_config_to_wire",
]
