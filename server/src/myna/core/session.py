"""Session configuration sent by the client when opening a transcription session.

Mirrors the IE114 request body, minus ``pipewire-node-name``: under the
audio-push model the client owns capture, so the service never needs a
PipeWire identifier. Audio format travels with the config so the service can
validate it against its advertised ``Capabilities.input_formats`` (the service
rejects mismatches rather than resampling — the client owns conversion).

This stays a flat config, not IE115's nested
``session.audio.input.transcription`` envelope —
that nesting exists to co-locate speech-to-speech output config (voice, speed,
output modalities) we don't have, so it's pure ceremony for a transcriber. There
is deliberately no ``turn_detection`` field: turn detection is client-driven
(the client signals end-of-audio explicitly), not server VAD. The wire
shape is versioned with ``myna.core.protocol.PROTOCOL_VERSION``.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any

from myna.core.audio import AudioFormat

#: Local dictionary limits (GitHub issue #1): entries a backend can be asked
#: to bias recognition toward, not a whole dictionary.
KEYWORDS_MAX_ENTRIES = 100
KEYWORDS_MAX_ENTRY_CHARS = 64


@dataclass(frozen=True)
class SessionConfig:
    audio_format: AudioFormat = field(default_factory=AudioFormat)
    language: str | None = None
    output_language: str | None = None
    prompt: str | None = None
    # "word" | "segment" | None — request timestamped segments on final/done.
    timestamp_granularity: str | None = None
    # Words/phrases (names, jargon) a backend biases recognition toward.
    # Structured, unlike the free-text `prompt`. Empty means no biasing.
    keywords: tuple[str, ...] = ()


def session_config_to_wire(config: SessionConfig) -> dict[str, Any]:
    return asdict(config)


def session_config_from_wire(wire: dict[str, Any]) -> SessionConfig:
    data = dict(wire)
    data["audio_format"] = AudioFormat(**data.get("audio_format") or {})
    if "keywords" in data:
        data["keywords"] = tuple(data["keywords"] or ())
    return SessionConfig(**data)


def normalize_keywords(entries: tuple[str, ...]) -> tuple[str, ...]:
    """Trim whitespace and drop empty entries; enforce the entry-count and
    per-entry length limits. Raises ``ValueError`` - the caller reports it
    through the dialect's own invalid-parameter error, since what "invalid"
    means is wire-specific."""
    trimmed = tuple(e.strip() for e in entries if e and e.strip())
    if len(trimmed) > KEYWORDS_MAX_ENTRIES:
        raise ValueError(f"{len(trimmed)} keywords, at most {KEYWORDS_MAX_ENTRIES} allowed")
    for entry in trimmed:
        if len(entry) > KEYWORDS_MAX_ENTRY_CHARS:
            raise ValueError(f"keyword {entry!r} exceeds {KEYWORDS_MAX_ENTRY_CHARS} characters")
    return trimmed
