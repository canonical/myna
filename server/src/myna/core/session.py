"""Session configuration sent by the client when opening a transcription session.

Mirrors the IE114 request body, minus ``pipewire-node-name``: under the
audio-push model the client owns capture, so the service never needs a
PipeWire identifier. Audio format travels with the config so the service can
validate it against its advertised ``Capabilities.input_formats`` (the service
rejects mismatches rather than resampling — the client owns conversion).

IE115 reconciliation (T37, see ``docs/IE115-deviations.md`` §1.6): this stays a
flat config, not IE115's nested ``session.audio.input.transcription`` envelope —
that nesting exists to co-locate speech-to-speech output config (voice, speed,
output modalities) we don't have, so it's pure ceremony for a transcriber. There
is deliberately no ``turn_detection`` field: turn detection is client-driven
(the client signals end-of-audio explicitly), not server VAD (§1.4). The wire
shape is versioned with ``myna.core.protocol.PROTOCOL_VERSION``.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any

from myna.core.audio import AudioFormat


@dataclass(frozen=True)
class SessionConfig:
    audio_format: AudioFormat = field(default_factory=AudioFormat)
    language: str | None = None
    output_language: str | None = None
    prompt: str | None = None
    # "word" | "segment" | None — request timestamped segments on final/done.
    timestamp_granularity: str | None = None


def session_config_to_wire(config: SessionConfig) -> dict[str, Any]:
    return asdict(config)


def session_config_from_wire(wire: dict[str, Any]) -> SessionConfig:
    data = dict(wire)
    data["audio_format"] = AudioFormat(**data.get("audio_format") or {})
    return SessionConfig(**data)
