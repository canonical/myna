"""Fetch a Chinese reference corpus from Google FLEURS (009 US1).

    uv run python dev/fetch_chinese_corpus.py [--out corpus/chinese] [-n 50]

Downloads the FLEURS Mandarin test split (``cmn_hans_cn``, CC-BY-4.0) directly
from the HF hub — ``test.tsv`` (metadata) + ``test.tar.gz`` (~525 MB of 16 kHz
mono WAVs) — selects the first N clips with duration ≥ 5 s, and writes a
manifest.csv matching the ``corpus/english/`` layout.

FLEURS is the standard multilingual ASR eval benchmark — a better fit than
Common Voice for comparing against published SenseVoice CER figures, and it's
not gated (Common Voice requires accepting terms on HF / tokenized S3 URLs).

Downloads are cached under ``.cache/`` and resumable; corpus output is
gitignored. Requires ``huggingface_hub`` (auto-installs on first use, like
modelscope in dev/fetch_funasr_model.py).
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
import tarfile
from io import BytesIO
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "server" / "src"))
from myna.testbed.corpus import stamp_corpus  # noqa: E402

RATE = 16_000
REPO_ROOT = Path(__file__).resolve().parent.parent
CACHE = REPO_ROOT / ".cache"
REPO_ID = "google/fleurs"
TSV_NAME = "data/cmn_hans_cn/test.tsv"
TAR_NAME = "data/cmn_hans_cn/audio/test.tar.gz"
LICENSE = "CC-BY-4.0"


def _install(lib: str, spec: str) -> None:
    print(f"`{lib}` not installed. Installing...", file=sys.stderr)
    installer = ["uv", "pip", "install", "--quiet", spec]
    try:
        subprocess.check_call(installer, stdout=sys.stderr)
    except (FileNotFoundError, subprocess.CalledProcessError):
        subprocess.check_call(
            [sys.executable, "-m", "pip", "install", spec],
            stdout=sys.stderr,
        )


# FLEURS TSV column 3 (normalized transcription) is inconsistent for
# Chinese: some rows space-separate CJK chars, some don't. Chinese CER is
# conventionally space-free, and SenseVoice emits Chinese unspaced — so
# drop spaces adjacent to CJK chars, keep latin word spacing intact.
_CJK = "\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff"
_CJK_SPACE_RE = re.compile(rf"(?<=[{_CJK}])\s+|\s+(?=[{_CJK}])")


def _clean_reference(text: str) -> str:
    return _CJK_SPACE_RE.sub("", text).strip()


def _download(name: str, cache: Path) -> Path:
    """hf_hub_download with a local .cache dir (resumable, reused across runs)."""
    try:
        from huggingface_hub import hf_hub_download  # type: ignore
    except ImportError:
        _install("huggingface_hub", "huggingface_hub>=0.24")
        from huggingface_hub import hf_hub_download  # type: ignore

    return Path(hf_hub_download(REPO_ID, name, repo_type="dataset", cache_dir=str(cache)))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "corpus" / "chinese")
    parser.add_argument("-n", type=int, default=50, help="Number of clips to select")
    args = parser.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    audio_dir = args.out / "audio"
    audio_dir.mkdir(exist_ok=True)

    print("⬇️  FLEURS cmn_hans_cn metadata...")
    tsv_path = _download(TSV_NAME, CACHE)
    print(f"⬇️  FLEURS cmn_hans_cn audio (~525 MB, cached under {CACHE})...")
    tar_path = _download(TAR_NAME, CACHE)

    # FLEURS TSV: id, filename.wav, raw_transcription, transcription,
    # char-split, num_samples, gender (tab-separated, no header row).
    # Column 3 (normalized transcription) is the right reference: latin spans
    # lowercased, punctuation removed — then unspaced at CJK boundaries.
    wanted: dict[str, str] = {}  # filename -> transcription
    with open(tsv_path, newline="", encoding="utf-8") as f:
        reader = csv.reader(f, delimiter="\t")
        for row in reader:
            if len(row) < 4:
                continue
            wanted[row[1]] = _clean_reference(row[3])

    print(f"   {len(wanted)} clips in test.tsv; selecting up to {args.n} ≥ 5 s")

    import soundfile as sf

    clips: list[dict] = []
    count = 0
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            if count >= args.n:
                break
            if not member.isfile() or not member.name.endswith(".wav"):
                continue
            name = Path(member.name).name
            reference = wanted.get(name)
            if reference is None:
                continue

            data = tar.extractfile(member).read()
            info = sf.info(BytesIO(data))
            duration_s = info.frames / info.samplerate
            if duration_s < 5.0:
                continue

            clip_id = f"fleurs-zh-{count:04d}"
            wav_path = audio_dir / f"{clip_id}.wav"

            # FLEURS wavs are 32-bit float — the harness's WavFileSource uses
            # stdlib wave (PCM-only), so always re-encode to 16 kHz mono
            # PCM s16. soundfile handles the read; ffmpeg only if the rate
            # or channel count actually differs.
            samples, sr = sf.read(BytesIO(data), dtype="float32", always_2d=True)
            if sr != RATE or info.channels != 1:
                ffmpeg = subprocess.run(
                    [
                        "ffmpeg",
                        "-loglevel",
                        "error",
                        "-f",
                        "f32le",
                        "-ar",
                        str(sr),
                        "-ac",
                        str(info.channels),
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
                    input=samples.tobytes(),
                    check=False,
                    capture_output=True,
                )
                if ffmpeg.returncode != 0:
                    print(f"⚠️  ffmpeg failed for {clip_id}", file=sys.stderr)
                    continue
                import numpy

                pcm16 = numpy.frombuffer(ffmpeg.stdout, dtype=numpy.int16)
                sf.write(str(wav_path), pcm16, RATE, subtype="PCM_16")
            else:
                sf.write(str(wav_path), samples[:, 0], RATE, subtype="PCM_16")

            clips.append(
                {
                    "id": clip_id,
                    "path": f"audio/{clip_id}.wav",
                    "text": reference,
                    "language": "zh",
                    "category": "non-english",
                    "duration_seconds": round(duration_s, 2),
                    "sample_rate_hz": RATE,
                    "channels": 1,
                    "source": f"https://huggingface.co/datasets/{REPO_ID} (cmn_hans_cn test)",
                    "license": "CC-BY-4.0",
                }
            )
            count += 1
            if count % 10 == 0:
                print(f"   {count}/{args.n} clips...")

    if count == 0:
        print("❌ no clips selected — check TSV/tar filename alignment", file=sys.stderr)
        return 1

    manifest_path = args.out / "manifest.json"
    manifest = {
        "schema_version": 1,
        "generator": "dev/fetch_chinese_corpus.py",
        "source": "google/fleurs cmn_hans_cn test split",
        "license": LICENSE,
        "clips": clips,
    }
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print(f"corpus id {stamp_corpus(manifest_path)}")

    notice_path = args.out / "README.txt"
    with open(notice_path, "w") as f:
        f.write(f"""\
Chinese reference corpus tier (009)

Derived from Google FLEURS (Mandarin, cmn_hans_cn test split), redistributed
under its original licence. Regenerate with:
uv run python dev/fetch_chinese_corpus.py

  Source:  https://huggingface.co/datasets/google/fleurs
  Licence: CC-BY-4.0  (https://creativecommons.org/licenses/by/4.0/)
  Cite:    Conneau et al., "FLEURS: Few-shot Learning Evaluation of Universal
           Representations of Speech", IEEE SLT 2022.
  Clips:   {count}
""")

    print(f"✅ {count} Chinese reference clips staged to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
