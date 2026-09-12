"""Build the Chinese reference corpus tier from Google FLEURS.

    myna-bench download-corpus-zh [--out corpus/chinese] [-n 50]

Downloads the FLEURS Mandarin test split (``cmn_hans_cn``, CC-BY-4.0) directly
from the HF hub - ``test.tsv`` (metadata) + ``test.tar.gz`` (~525 MB of 16 kHz
WAVs) - selects the first N clips of at least 5 s, and writes a manifest in the
same schema as the English tier.

FLEURS is the standard multilingual ASR eval benchmark - a better fit than
Common Voice for comparing against published SenseVoice CER figures, and it is
not gated (Common Voice requires accepting terms on HF / tokenized S3 URLs).

Unlike the English tier this needs ``huggingface_hub``, ``soundfile`` and
``numpy``. They are imported lazily and reported, never installed: a benchmark
tool that mutates the environment it is measuring is exactly the surprise
``guard`` exists to refuse.
"""

from __future__ import annotations

import csv
import json
import re
import subprocess
import sys
import tarfile
from io import BytesIO
from pathlib import Path

from myna.testbed.corpus import stamp_corpus

RATE = 16_000
REPO_ID = "google/fleurs"
TSV_NAME = "data/cmn_hans_cn/test.tsv"
TAR_NAME = "data/cmn_hans_cn/audio/test.tar.gz"
LICENSE = "CC-BY-4.0"

# FLEURS TSV column 3 (normalized transcription) is inconsistent for Chinese:
# some rows space-separate CJK chars, some don't. Chinese CER is conventionally
# space-free, and SenseVoice emits Chinese unspaced - so drop spaces adjacent
# to CJK chars, keep latin word spacing intact.
_CJK = "\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff"
_CJK_SPACE_RE = re.compile(rf"(?<=[{_CJK}])\s+|\s+(?=[{_CJK}])")


def _clean_reference(text: str) -> str:
    return _CJK_SPACE_RE.sub("", text).strip()


def _require(module: str, install: str):
    try:
        return __import__(module)
    except ImportError as exc:
        raise SystemExit(
            f"the Chinese tier needs {module!r}, which is not installed.\n"
            f"  pip install {install}\n"
            "(the English tier needs only ffmpeg, and is the default corpus.)"
        ) from exc


def _download(name: str, cache: Path) -> Path:
    """hf_hub_download into a local cache dir (resumable, reused across runs)."""
    _require("huggingface_hub", "huggingface_hub>=0.24")
    from huggingface_hub import hf_hub_download

    return Path(hf_hub_download(REPO_ID, name, repo_type="dataset", cache_dir=str(cache)))


def cmd_download_zh(args) -> None:  # noqa: ANN001
    out = Path(args.out)
    cache = Path(args.cache)
    limit: int = args.n

    out.mkdir(parents=True, exist_ok=True)
    audio_dir = out / "audio"
    audio_dir.mkdir(exist_ok=True)

    print("FLEURS cmn_hans_cn metadata...")
    tsv_path = _download(TSV_NAME, cache)
    print(f"FLEURS cmn_hans_cn audio (~525 MB, cached under {cache})...")
    tar_path = _download(TAR_NAME, cache)

    # FLEURS TSV: id, filename.wav, raw_transcription, transcription,
    # char-split, num_samples, gender (tab-separated, no header row).
    # Column 3 (normalized transcription) is the right reference: latin spans
    # lowercased, punctuation removed - then unspaced at CJK boundaries.
    wanted: dict[str, str] = {}
    with tsv_path.open(newline="", encoding="utf-8") as fp:
        for row in csv.reader(fp, delimiter="\t"):
            if len(row) >= 4:
                wanted[row[1]] = _clean_reference(row[3])
    print(f"   {len(wanted)} clips in test.tsv; selecting up to {limit} of >= 5 s")

    _require("soundfile", "soundfile")
    import soundfile as sf

    clips: list[dict] = []
    count = 0
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            if count >= limit:
                break
            if not member.isfile() or not member.name.endswith(".wav"):
                continue
            reference = wanted.get(Path(member.name).name)
            if reference is None:
                continue

            extracted = tar.extractfile(member)
            if extracted is None:
                continue
            data = extracted.read()
            info = sf.info(BytesIO(data))
            duration_s = info.frames / info.samplerate
            if duration_s < 5.0:
                continue

            clip_id = f"fleurs-zh-{count:04d}"
            wav_path = audio_dir / f"{clip_id}.wav"

            # FLEURS wavs are 32-bit float - the harness's WavFileSource uses
            # stdlib wave (PCM-only), so always re-encode to 16 kHz mono PCM
            # s16. soundfile handles the read; ffmpeg only if the rate or
            # channel count actually differs.
            samples, sample_rate = sf.read(BytesIO(data), dtype="float32", always_2d=True)
            if sample_rate != RATE or info.channels != 1:
                ffmpeg = subprocess.run(
                    [
                        "ffmpeg",
                        "-loglevel",
                        "error",
                        "-f",
                        "f32le",
                        "-ar",
                        str(sample_rate),
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
                    print(f"ffmpeg failed for {clip_id}", file=sys.stderr)
                    continue
                _require("numpy", "numpy")
                import numpy

                sf.write(
                    str(wav_path),
                    numpy.frombuffer(ffmpeg.stdout, dtype=numpy.int16),
                    RATE,
                    subtype="PCM_16",
                )
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
                    "license": LICENSE,
                }
            )
            count += 1
            if count % 10 == 0:
                print(f"   {count}/{limit} clips...")

    if count == 0:
        raise SystemExit("no clips selected - check TSV/tar filename alignment")

    manifest_path = out / "manifest.json"
    manifest_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generator": "myna-bench download-corpus-zh",
                "source": "google/fleurs cmn_hans_cn test split",
                "license": LICENSE,
                "clips": clips,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"corpus id {stamp_corpus(manifest_path)}")

    (out / "README.txt").write_text(
        "Chinese reference corpus tier\n"
        "\n"
        "Derived from Google FLEURS (Mandarin, cmn_hans_cn test split),\n"
        "redistributed under its original licence. Regenerate with:\n"
        "  myna-bench download-corpus-zh\n"
        "\n"
        f"  Source:  https://huggingface.co/datasets/{REPO_ID}\n"
        f"  Licence: {LICENSE}  (https://creativecommons.org/licenses/by/4.0/)\n"
        '  Cite:    Conneau et al., "FLEURS: Few-shot Learning Evaluation of Universal\n'
        '           Representations of Speech", IEEE SLT 2022.\n'
        f"  Clips:   {count}\n",
        encoding="utf-8",
    )
    print(f"{count} Chinese reference clips staged to {out}")
