"""Build the real recorded-speech corpus tier (T25).

    uv run python dev/fetch_real_corpus.py [--out corpus/real] [-n 12]

Real human speech with exact reference transcripts, so WER is trustworthy — the
synthetic espeak tier is out-of-distribution and its WER is misleading across
architectures (Nemotron ~0% on real voice vs ~45% on espeak; plan T09/T25).

Source: LibriSpeech dev-clean (Panayotov et al., ICASSP 2015), CC-BY-4.0, real
read English at 16 kHz. The ~337 MB download is cached under .cache/; the corpus
is regenerated on demand, not committed (gitignored like fixtures/).

Selection is deliberately trivial: the first N utterances in archive order,
tagged "quiet" (clean read speech), plus a couple of seeded-noise variants.
Speaker/accent diversity is a follow-up (needs an accent-labelled corpus).

Requires ffmpeg (FLAC decode). Network is needed only for the download.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tarfile
import urllib.request
from array import array
from pathlib import Path

# Reuse the synthetic tier's WAV writer + seeded-noise mixer (same house format).
sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate_fixtures import (  # noqa: E402
    NOISE_SEED,
    NOISE_SNR_DB,
    mix_noise,
    write_wav,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
RATE = 16_000
URL = "https://www.openslr.org/resources/12/dev-clean.tar.gz"
PREFIX = "LibriSpeech/dev-clean/"
LICENSE = "CC-BY-4.0"
N_NOISE = 2

NOTICE = f"""\
Real recorded-speech corpus tier (T25)

Derived from the LibriSpeech ASR corpus (dev-clean), redistributed under its
original licence. Regenerate with: uv run python dev/fetch_real_corpus.py

  Source:  {URL}
  Licence: CC-BY-4.0  (https://creativecommons.org/licenses/by/4.0/)
  Cite:    V. Panayotov, G. Chen, D. Povey, S. Khudanpur, "Librispeech: an ASR
           corpus based on public domain audio books", ICASSP 2015.

Each clip's source utterance id and licence are in manifest.json. Audio is
decoded to 16 kHz mono S16LE WAV; "noise" clips add seeded Gaussian noise at
{NOISE_SNR_DB:.0f} dB SNR.
"""


def download(url: str, dest: Path) -> Path:
    if dest.exists() and dest.stat().st_size:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {url} (~337 MB)\n  -> {dest}")
    with urllib.request.urlopen(url) as resp, dest.open("wb") as out:  # noqa: S310
        while block := resp.read(1 << 20):
            out.write(block)
    return dest


def decode_flac(data: bytes) -> array:
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
    return array("h", pcm)


def collect(tar_path: Path, n: int) -> list[tuple[str, array, str]]:
    """The first ``n`` utterances in archive order, with their transcripts."""
    pcm: dict[str, array] = {}
    text: dict[str, str] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(PREFIX)):
                continue
            if name.endswith(".trans.txt"):
                for line in tar.extractfile(member).read().decode().splitlines():
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac") and len(pcm) < n:
                pcm[Path(name).stem] = decode_flac(tar.extractfile(member).read())
    return [(uid, pcm[uid], text[uid]) for uid in pcm if uid in text]


def build(out_dir: Path, tar_path: Path, n: int) -> Path:
    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    clips = collect(tar_path, n)
    if not clips:
        raise SystemExit("no clips selected — is this the LibriSpeech dev-clean tarball?")

    entries: list[dict] = []

    def add(clip_id: str, samples: array, txt: str, category: str, source: str) -> None:
        duration = write_wav(audio_dir / f"{clip_id}.wav", samples, RATE)
        entries.append(
            {
                "id": clip_id,
                "path": f"audio/{clip_id}.wav",
                "text": txt,
                "language": "en",
                "category": category,
                "duration_seconds": round(duration, 3),
                "sample_rate_hz": RATE,
                "channels": 1,
                "source": source,
                "license": LICENSE,
            }
        )
        print(f"  {clip_id:<30} {category:<6} {duration:6.2f}s")

    for utt_id, samples, txt in clips:
        add(
            f"librispeech-{utt_id}",
            samples,
            txt,
            "quiet",
            f"librispeech:dev-clean:{utt_id}",
        )
    for utt_id, samples, txt in clips[:N_NOISE]:
        add(
            f"librispeech-{utt_id}-noise-snr{int(NOISE_SNR_DB)}",
            mix_noise(samples, NOISE_SNR_DB, NOISE_SEED),
            txt,
            "noise",
            f"librispeech:dev-clean:{utt_id}+noise",
        )

    (out_dir / "NOTICE").write_text(NOTICE, encoding="utf-8")
    manifest = out_dir / "manifest.json"
    manifest.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generator": "dev/fetch_real_corpus.py",
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "corpus" / "real")
    parser.add_argument("--cache", type=Path, default=REPO_ROOT / ".cache" / "librispeech")
    parser.add_argument(
        "--tarball",
        type=Path,
        default=None,
        help="use an already-downloaded dev-clean.tar.gz instead of fetching",
    )
    parser.add_argument("-n", type=int, default=12, help="number of clean clips (default 12)")
    args = parser.parse_args()

    tar_path = args.tarball or download(URL, args.cache / "dev-clean.tar.gz")
    print(f"\nwrote {build(args.out, tar_path, args.n)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
