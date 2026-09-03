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

import hashlib
import json
from collections.abc import Iterable
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
    sha256: str  # audio digest, as recorded in the manifest
    voice: str | None = None  # synthesis voice, when synthetic

    def open_source(self, *, chunk_seconds: float = 0.1, realtime: bool = False) -> WavFileSource:
        return WavFileSource(self.path, chunk_seconds=chunk_seconds, realtime=realtime)


def load_manifest(manifest_path: Path | str) -> tuple[Clip, ...]:
    manifest_path = Path(manifest_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    version = manifest.get("schema_version")
    if version != SCHEMA_VERSION:
        raise ValueError(
            f"{manifest_path}: schema_version {version!r} unsupported (expected {SCHEMA_VERSION})"
        )
    clips = []
    for entry in manifest["clips"]:
        entry = dict(entry)
        if not entry.get("sha256"):
            raise ValueError(
                f"{manifest_path}: clip {entry.get('id')!r} records no sha256 - "
                "regenerate the corpus"
            )
        entry["path"] = (manifest_path.parent / entry["path"]).resolve()
        clips.append(Clip(**entry))
    return tuple(clips)


def by_category(clips: tuple[Clip, ...]) -> dict[str, tuple[Clip, ...]]:
    grouped: dict[str, list[Clip]] = {}
    for clip in clips:
        grouped.setdefault(clip.category, []).append(clip)
    return {category: tuple(group) for category, group in grouped.items()}


CORPUS_ID_VERSION = 1


def sha256_file(path: Path | str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fp:
        for block in iter(lambda: fp.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def digest_files(paths: Iterable[Path | str]) -> str:
    """One digest over a set of files, by name."""
    h = hashlib.sha256()
    for path in sorted(Path(p) for p in paths):
        h.update(f"{path.name}\0{sha256_file(path)}\n".encode())
    return h.hexdigest()


def _corpus_id(manifest: dict, where: Path) -> str:
    h = hashlib.sha256()
    for entry in sorted(manifest["clips"], key=lambda c: c["id"]):
        if not entry.get("sha256"):
            raise ValueError(
                f"{where}: clip {entry.get('id')!r} records no sha256 - regenerate the corpus"
            )
        h.update(
            "\0".join(
                (
                    entry["id"],
                    entry["sha256"],
                    entry["text"],
                    entry["language"],
                    entry["category"],
                )
            ).encode("utf-8")
        )
        h.update(b"\n")
    return f"v{CORPUS_ID_VERSION}:{h.hexdigest()[:16]}"


def corpus_id(manifest_path: Path | str) -> str:
    """Identity of the corpus a manifest describes, from the manifest alone:
    the clips it names, the audio digests it records, and the reference text
    those clips are scored against. Numbers are comparable when ids match."""
    manifest_path = Path(manifest_path)
    return _corpus_id(json.loads(manifest_path.read_text(encoding="utf-8")), manifest_path)


def verify_corpus(manifest_path: Path | str) -> str:
    """The corpus id, with every clip on disk hashed against what the manifest
    records. Raises naming the clips that differ: audio that is not what the
    manifest says it is has to stop a run, not quietly score one."""
    manifest_path = Path(manifest_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    identity = corpus_id(manifest_path)
    bad = []
    for entry in manifest["clips"]:
        audio = (manifest_path.parent / entry["path"]).resolve()
        if not audio.is_file():
            bad.append(f"{entry['id']}: missing ({audio})")
        elif (found := sha256_file(audio)) != entry["sha256"]:
            bad.append(f"{entry['id']}: {found[:16]} != {entry['sha256'][:16]}")
    if bad:
        raise ValueError(
            f"{manifest_path}: {len(bad)} clip(s) are not what the manifest records - "
            "regenerate the corpus\n  " + "\n  ".join(bad)
        )
    recorded = manifest.get("corpus_id")
    if recorded is not None and recorded != identity:
        raise ValueError(
            f"{manifest_path} is stamped {recorded} but describes {identity} - "
            "regenerate the corpus, or restore the tier the stamp names"
        )
    return identity


def stamp_corpus(manifest_path: Path | str) -> str:
    """Record each clip's audio digest and the corpus id they add up to.
    Refuses to relabel a clip whose recorded digest disagrees with its bytes:
    re-stamping in place is how a corpus stops being the one results name."""
    manifest_path = Path(manifest_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    for entry in manifest["clips"]:
        found = sha256_file((manifest_path.parent / entry["path"]).resolve())
        if entry.get("sha256") and entry["sha256"] != found:
            raise ValueError(
                f"{manifest_path}: clip {entry['id']!r} is {found[:16]}, "
                f"not the recorded {entry['sha256'][:16]}"
            )
        entry["sha256"] = found
    identity = _corpus_id(manifest, manifest_path)
    stamped = {k: v for k, v in manifest.items() if k != "clips"}
    stamped["corpus_id"] = identity
    stamped["clips"] = manifest["clips"]
    manifest_path.write_text(
        json.dumps(stamped, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return identity
