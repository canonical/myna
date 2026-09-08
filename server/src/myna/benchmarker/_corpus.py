"""``make-corpus``: build a manifest from a directory of your own recordings.

Downloading a published corpus lives in ``corpus_english`` (LibriSpeech) and
``corpus_chinese`` (FLEURS). This module covers the other direction: a tester
with their own WAVs and reference transcripts, benchmarked against the same
schema and the same ``corpus_id`` stamping, so their numbers are comparable to
each other even though the audio is not ours to publish.
"""

from __future__ import annotations

import json
from pathlib import Path

from myna.benchmarker._audio import read_wav_meta
from myna.testbed.corpus import stamp_corpus

SCHEMA_VERSION = 1


def cmd_make(args) -> None:  # noqa: ANN001
    src_dir = Path(args.dir)
    out_dir = Path(args.out) if args.out else src_dir
    language: str = args.language
    default_category: str = args.category

    if not src_dir.is_dir():
        raise SystemExit(f"not a directory: {src_dir}")

    wavs = sorted(src_dir.glob("*.wav"))
    if not wavs:
        raise SystemExit(f"no *.wav files found in {src_dir}")

    entries: list[dict] = []
    skipped: list[str] = []
    for wav in wavs:
        txt_path = wav.with_suffix(".txt")
        if not txt_path.exists():
            skipped.append(wav.name)
            continue
        text = txt_path.read_text(encoding="utf-8").strip()
        if not text:
            skipped.append(wav.name)
            continue
        category_path = wav.with_suffix(".category")
        category = (
            category_path.read_text(encoding="utf-8").strip()
            if category_path.exists()
            else default_category
        )
        try:
            duration, rate, channels = read_wav_meta(wav)
        except Exception as exc:  # noqa: BLE001
            print(f"  skipping {wav.name}: {exc}")
            skipped.append(wav.name)
            continue

        clip_id = wav.stem
        # Relative to the manifest, which is what load_manifest resolves against:
        # an --out beside the clips rather than over them still has to load.
        rel = wav.relative_to(out_dir, walk_up=True)
        entries.append(
            {
                "id": clip_id,
                "path": str(rel),
                "text": text,
                "language": language,
                "category": category,
                "duration_seconds": round(duration, 3),
                "sample_rate_hz": rate,
                "channels": channels,
                "source": f"user-provided:{wav.name}",
                "license": "unknown",
            }
        )
        print(f"  {clip_id:<40} {category}  {duration:.2f}s")

    if not entries:
        raise SystemExit(
            "no clips added - make sure each .wav has a matching .txt sidecar with the transcript"
        )

    out_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "generator": "myna-bench make-corpus",
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"\ncorpus id {stamp_corpus(manifest_path)}")
    print(f"added {len(entries)} clips, skipped {len(skipped)}")
    if skipped:
        print(f"skipped (no .txt sidecar or unreadable): {', '.join(skipped)}")
    print(f"wrote {manifest_path}")
    print(f"Use in bench.yaml:  manifest: {manifest_path}")
