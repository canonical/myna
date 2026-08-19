"""Corpus subcommands: download-corpus and make-corpus.

``download-corpus`` fetches a balanced clip set from LibriSpeech (CC-BY-4.0)
and writes a manifest.json ready for use with bench.yaml.  It has no
dependencies beyond the stdlib + ffmpeg (for FLAC decode) — no numpy.

``make-corpus`` walks a directory of WAV files and sidecars and produces a
manifest.json using the same schema, so testers can benchmark against their
own recordings.
"""

from __future__ import annotations

import array
import json
import subprocess
import tarfile
import urllib.request
import wave
from pathlib import Path

RATE = 16_000
BASE_URL = "https://www.openslr.org/resources/12"
SUBSETS = ("dev-clean", "dev-other", "test-clean", "test-other")
LICENSE = "CC-BY-4.0"
SCHEMA_VERSION = 1


# ---------------------------------------------------------------------------
# Download helpers
# ---------------------------------------------------------------------------


def _download(url: str, dest: Path) -> Path:
    if dest.exists() and dest.stat().st_size:
        print(f"using cached {dest}")
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {url}  (~330 MB)\n  -> {dest}")
    with urllib.request.urlopen(url) as resp, dest.open("wb") as out:  # noqa: S310
        total = int(resp.headers.get("Content-Length") or 0)
        received = 0
        while block := resp.read(1 << 20):
            out.write(block)
            received += len(block)
            if total:
                pct = received / total * 100
                print(f"  {pct:5.1f}%  {received >> 20} / {total >> 20} MB\r", end="", flush=True)
    print()
    return dest


def _decode_flac(data: bytes) -> array.array:
    """Decode FLAC bytes to 16 kHz mono S16LE samples via ffmpeg (piped)."""
    pcm = subprocess.run(
        [
            "ffmpeg",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-ar",
            str(RATE),
            "-ac",
            "1",
            "-f",
            "s16le",
            "pipe:1",
        ],
        input=data,
        stdout=subprocess.PIPE,
        check=True,
    ).stdout
    return array.array("h", pcm)


def _write_wav(path: Path, samples: array.array, rate: int) -> float:
    with wave.open(str(path), "w") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(rate)
        wf.writeframes(samples.tobytes())
    return len(samples) / rate


def _speaker(utt_id: str) -> str:
    return utt_id.split("-", 1)[0]


def _round_robin(by_speaker: dict[str, list[str]], n: int) -> list[str]:
    picked: list[str] = []
    depth = 0
    while len(picked) < n:
        row = [utts[depth] for utts in by_speaker.values() if depth < len(utts)]
        if not row:
            break
        picked.extend(row[: n - len(picked)])
        depth += 1
    return picked


def _build_corpus(out_dir: Path, tar_path: Path, n: int, subset: str) -> Path:
    """Extract ``n`` balanced clips from ``tar_path`` and write manifest.json."""
    prefix = f"LibriSpeech/{subset}/"
    category = "accent" if subset.endswith("-other") else "quiet"
    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)

    # Pass 1: index transcripts and utterance ids by speaker.
    print("indexing archive …")
    text: dict[str, str] = {}
    by_speaker: dict[str, list[str]] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(prefix)):
                continue
            if name.endswith(".trans.txt"):
                for line in tar.extractfile(member).read().decode().splitlines():  # type: ignore[union-attr]
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac"):
                utt_id = Path(name).stem
                by_speaker.setdefault(_speaker(utt_id), []).append(utt_id)

    by_speaker = {
        spk: sorted(utts) for spk, utts in sorted(by_speaker.items(), key=lambda item: int(item[0]))
    }
    wanted = set(_round_robin(by_speaker, n)) & set(text)
    print(f"selected {len(wanted)} clips across {len({_speaker(u) for u in wanted})} speakers")

    # Pass 2: decode only the selected clips.
    print("decoding clips …")
    pcm: dict[str, array.array] = {}
    remaining = set(wanted)
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            if not (member.isfile() and member.name.endswith(".flac")):
                continue
            utt_id = Path(member.name).stem
            if utt_id in remaining:
                pcm[utt_id] = _decode_flac(tar.extractfile(member).read())  # type: ignore[arg-type]
                remaining.discard(utt_id)
                print(f"  decoded {utt_id}")
                if not remaining:
                    break

    entries: list[dict] = []
    for utt_id in sorted(pcm):
        clip_id = f"librispeech-{utt_id}"
        wav_path = audio_dir / f"{clip_id}.wav"
        duration = _write_wav(wav_path, pcm[utt_id], RATE)
        entries.append(
            {
                "id": clip_id,
                "path": f"audio/{clip_id}.wav",
                "text": text[utt_id],
                "language": "en",
                "category": category,
                "duration_seconds": round(duration, 3),
                "sample_rate_hz": RATE,
                "channels": 1,
                "source": f"librispeech:{subset}:{utt_id}",
                "license": LICENSE,
            }
        )
        print(f"  {clip_id:<44} {category}  {duration:.2f}s")

    notice = (
        f"LibriSpeech corpus ({subset}), CC-BY-4.0\n"
        f"Source: {BASE_URL}/{subset}.tar.gz\n"
        "Cite: V. Panayotov et al., ICASSP 2015\n"
    )
    (out_dir / "NOTICE").write_text(notice, encoding="utf-8")

    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "generator": "myna-bench download-corpus",
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    return manifest_path


# ---------------------------------------------------------------------------
# WAV metadata
# ---------------------------------------------------------------------------


def _read_wav_meta(path: Path) -> tuple[float, int, int]:
    """Return (duration_seconds, sample_rate_hz, channels) for a WAV file."""
    with wave.open(str(path), "r") as wf:
        frames = wf.getnframes()
        rate = wf.getframerate()
        channels = wf.getnchannels()
    duration = frames / rate if rate else 0.0
    return round(duration, 3), rate, channels


# ---------------------------------------------------------------------------
# Subcommand implementations
# ---------------------------------------------------------------------------


def cmd_download(args) -> None:  # noqa: ANN001
    out_dir = Path(args.out)
    cache_dir = Path(args.cache)
    n: int = args.n
    subset: str = args.subset

    # Check ffmpeg is available before downloading anything.
    try:
        subprocess.run(["ffmpeg", "-version"], capture_output=True, check=True)
    except (OSError, subprocess.SubprocessError) as err:
        raise SystemExit("ffmpeg is required for FLAC decode: sudo apt install ffmpeg") from err

    tar_path = _download(f"{BASE_URL}/{subset}.tar.gz", cache_dir / f"{subset}.tar.gz")
    manifest = _build_corpus(out_dir, tar_path, n, subset)
    print(f"\nwrote {manifest}")
    print(f"Use in bench.yaml:  manifest: {out_dir}/manifest.json")


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
            duration, rate, channels = _read_wav_meta(wav)
        except Exception as exc:  # noqa: BLE001
            print(f"  skipping {wav.name}: {exc}")
            skipped.append(wav.name)
            continue

        clip_id = wav.stem
        rel = wav.relative_to(out_dir) if wav.is_relative_to(out_dir) else Path(wav.name)
        entries.append(
            {
                "id": clip_id,
                "path": str(rel),
                "text": text,
                "language": language,
                "category": category,
                "duration_seconds": duration,
                "sample_rate_hz": rate,
                "channels": channels,
                "source": f"user-provided:{wav.name}",
                "license": "unknown",
            }
        )
        print(f"  {clip_id:<40} {category}  {duration:.2f}s")

    if not entries:
        raise SystemExit(
            "no clips added — make sure each .wav has a matching .txt sidecar with the transcript"
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
    print(f"\nadded {len(entries)} clips, skipped {len(skipped)}")
    if skipped:
        print(f"skipped (no .txt sidecar or unreadable): {', '.join(skipped)}")
    print(f"wrote {manifest_path}")
    print(f"Use in bench.yaml:  manifest: {manifest_path}")
