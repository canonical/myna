"""Session configuration sent by the client when opening a transcription session.

Mirrors the IE114 request body, minus ``pipewire-node-name``: under the
audio-push model the client owns capture, so the service never needs a
PipeWire identifier. Audio format travels with the config so the service can
validate or resample before inference.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from myna.core.audio import AudioFormat


@dataclass(frozen=True)
class SessionConfig:
    audio_format: AudioFormat = field(default_factory=AudioFormat)
    language: str | None = None
    output_language: str | None = None
    prompt: str | None = None
    # "word" | "segment" | None — request timestamped segments on final/done.
    timestamp_granularity: str | None = None
