"""Fixture corpus: audio clips with reference transcripts.

A corpus is a directory holding ``manifest.json`` plus the audio files it
references. Every clip carries its reference text (for WER scoring, T06), the
UD129 accuracy-matrix category it covers, and license/source provenance —
redistribution must be checkable from the manifest alone.

The synthetic tier is generated locally by ``dev/generate_fixtures.py``
(espeak-ng); a real-speech tier with recorded audio is tracked separately in
the project plan. Fixture audio is read-only test data — never recordings of
users, and never written to by the harness.

Categories (from the UD129 accuracy test matrix):
quiet, noise, accent, non-english, commands, long-form, technical, names,
acronyms.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from myna.testbed.sources import WavFileSource

SCHEMA_VERSION = 1


@dataclass(frozen=True)
class Clip:
    id: str
    path: Path  # absolute, resolved against the manifest location
    text: str  # reference transcript
    language: str  # BCP-47-ish, e.g. "en", "en-GB", "de"
    category: str
    duration_seconds: float
    sample_rate_hz: int
    channels: int
    source: str  # provenance, e.g. "synthetic:espeak-ng" or a URL
    license: str  # SPDX identifier
    voice: str | None = None  # synthesis voice, when synthetic

    def open_source(self, *, chunk_seconds: float = 0.1, realtime: bool = False) -> WavFileSource:
        return WavFileSource(self.path, chunk_seconds=chunk_seconds, realtime=realtime)


def load_manifest(manifest_path: Path | str) -> tuple[Clip, ...]:
    manifest_path = Path(manifest_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    version = manifest.get("schema_version")
    if version != SCHEMA_VERSION:
        raise ValueError(
            f"{manifest_path}: schema_version {version!r} unsupported "
            f"(expected {SCHEMA_VERSION})"
        )
    clips = []
    for entry in manifest["clips"]:
        entry = dict(entry)
        entry["path"] = (manifest_path.parent / entry["path"]).resolve()
        clips.append(Clip(**entry))
    return tuple(clips)


def by_category(clips: tuple[Clip, ...]) -> dict[str, tuple[Clip, ...]]:
    grouped: dict[str, list[Clip]] = {}
    for clip in clips:
        grouped.setdefault(clip.category, []).append(clip)
    return {category: tuple(group) for category, group in grouped.items()}
