"""Build the English recorded-speech corpus tier (T25).

    uv run python dev/fetch_english_corpus.py [--out corpus/english] [-n 12]

Real English human speech with exact reference transcripts, so WER is trustworthy — the
synthetic espeak tier is out-of-distribution and its WER is misleading across
architectures (Nemotron ~0% on real voice vs ~45% on espeak; plan T09/T25).

Source: LibriSpeech (Panayotov et al., ICASSP 2015), CC-BY-4.0, real read English
at 16 kHz. ``--subset`` picks the split: the ``-clean`` ones are well-recorded
speech, the ``-other`` ones LibriSpeech's deliberately harder half (accented,
noisier, lower-fidelity) - the pair papers quote WER on. One split per output
dir, so an ``-other`` tier needs its own ``--out``. Each ~330 MB download is
cached under .cache/; corpora are regenerated on demand, not committed
(gitignored like fixtures/).

Two selection strategies (``--select``):

``archive`` (the original, kept so ``manifest.json`` reproduces bit-for-bit)
    the first N utterances in archive order. Cheap, but dev-clean's archive
    order means all N land on *one* speaker - a WER computed over it measures
    one voice, not the language.

``balanced`` (use this for accuracy benchmarks)
    round-robin over every speaker in the split (40 in dev-clean, 33 in
    test-other), taking utterances in sorted order per speaker, so N clips
    spread evenly across speakers and - since LibriSpeech's eval splits are
    sex-balanced - roughly evenly across M/F.

Either way a couple of seeded-noise variants are appended. Speaker id is
recoverable from the clip id (``librispeech-<speaker>-<chapter>-<utt>``), so
per-speaker WER can be broken out without extra manifest fields. Accent
diversity beyond the ``-other`` splits is still a follow-up (LibriSpeech is
overwhelmingly US English; it needs an accent-labelled corpus such as Common
Voice or EdAcc).

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
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "server" / "src"))
from generate_fixtures import (  # noqa: E402
    NOISE_SEED,
    NOISE_SNR_DB,
    mix_noise,
    write_wav,
)
from myna.testbed.corpus import stamp_corpus  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent
RATE = 16_000
BASE_URL = "https://www.openslr.org/resources/12"
# The LibriSpeech splits worth sweeping. "clean" is well-recorded read speech;
# "other" is the deliberately harder half (accented, noisier, lower-fidelity
# recordings) — the pair papers report WER on, so numbers here are comparable
# to published figures.
SUBSETS = ("dev-clean", "dev-other", "test-clean", "test-other")
LICENSE = "CC-BY-4.0"
N_NOISE = 2


def notice_for(subset: str) -> str:
    return f"""\
Real recorded-speech corpus tier (T25)

Derived from the LibriSpeech ASR corpus ({subset}), redistributed under its
original licence. Regenerate with: uv run python dev/fetch_english_corpus.py

  Source:  {BASE_URL}/{subset}.tar.gz
  Licence: CC-BY-4.0  (https://creativecommons.org/licenses/by/4.0/)
  Cite:    V. Panayotov, G. Chen, D. Povey, S. Khudanpur, "Librispeech: an ASR
           corpus based on public domain audio books", ICASSP 2015.

Each clip's source utterance id and licence are in the manifest. Audio is
decoded to 16 kHz mono S16LE WAV; "noise" clips add seeded Gaussian noise at
{NOISE_SNR_DB:.0f} dB SNR.
"""


def download(url: str, dest: Path) -> Path:
    if dest.exists() and dest.stat().st_size:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {url} (~330 MB per split)\n  -> {dest}")
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


def collect(tar_path: Path, n: int, prefix: str) -> list[tuple[str, array, str]]:
    """The first ``n`` utterances in archive order, with their transcripts."""
    pcm: dict[str, array] = {}
    text: dict[str, str] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(prefix)):
                continue
            if name.endswith(".trans.txt"):
                for line in tar.extractfile(member).read().decode().splitlines():
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac") and len(pcm) < n:
                pcm[Path(name).stem] = decode_flac(tar.extractfile(member).read())
    return [(uid, pcm[uid], text[uid]) for uid in pcm if uid in text]


def _speaker(utt_id: str) -> str:
    """``2277-149896-0026`` -> ``2277``."""
    return utt_id.split("-", 1)[0]


def _chapter(utt_id: str) -> str:
    """``2277-149896-0026`` -> ``2277-149896``."""
    speaker, chapter, _ = utt_id.split("-", 2)
    return f"{speaker}-{chapter}"


def _by_id(item: tuple[str, list[str]]) -> int:
    """Sort speakers numerically, so 84 precedes 174 (str order would not)."""
    return int(item[0])


def _round_robin(by_speaker: dict[str, list[str]], n: int) -> list[str]:
    """Take one utterance per speaker per pass, speakers in sorted id order.

    Deterministic, and it degrades sanely: with fewer speakers than ``n`` it
    wraps for a second (third, ...) utterance each; with more speakers than
    ``n`` it takes one each from the lowest-numbered speakers.
    """
    picked: list[str] = []
    depth = 0
    while len(picked) < n:
        row = [utts[depth] for utts in by_speaker.values() if depth < len(utts)]
        if not row:  # exhausted every speaker
            break
        picked.extend(row[: n - len(picked)])
        depth += 1
    return picked


def collect_balanced(tar_path: Path, n: int, prefix: str) -> list[tuple[str, array, str]]:
    """``n`` utterances spread round-robin over every speaker in the split.

    Two passes over the tarball: the first indexes utterance ids and
    transcripts (cheap — no FLAC decode), the second decodes only the
    selected members. A gzip stream can't be seeked, hence the reopen.
    """
    text: dict[str, str] = {}
    by_speaker: dict[str, list[str]] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(prefix)):
                continue
            if name.endswith(".trans.txt"):
                for line in tar.extractfile(member).read().decode().splitlines():
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac"):
                utt_id = Path(name).stem
                by_speaker.setdefault(_speaker(utt_id), []).append(utt_id)

    by_speaker = {spk: sorted(utts) for spk, utts in sorted(by_speaker.items(), key=_by_id)}
    wanted = [uid for uid in _round_robin(by_speaker, n) if uid in text]
    print(f"selected {len(wanted)} clips across {len({_speaker(u) for u in wanted})} speakers")

    remaining = set(wanted)
    pcm: dict[str, array] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            if not (member.isfile() and member.name.endswith(".flac")):
                continue
            utt_id = Path(member.name).stem
            if utt_id in remaining:
                pcm[utt_id] = decode_flac(tar.extractfile(member).read())
                remaining.discard(utt_id)
                if not remaining:
                    break
    return [(uid, pcm[uid], text[uid]) for uid in wanted if uid in pcm]


# Gap of digital silence spliced between concatenated utterances: long enough
# to read as a natural pause (so a streaming adapter's endpointer sees a real
# boundary, not a click), short enough not to inflate the target duration.
LONG_FORM_GAP_SECONDS = 0.4


def long_form_entry(out_dir: Path, tar_path: Path, minutes: float, subset: str) -> dict:
    """One continuous clip: a whole LibriSpeech chapter, read in order.

    Individual LibriSpeech utterances are single sentences (a few seconds
    each) — no clip in the per-utterance tiers exercises a long dictation
    session, and none is long enough to exercise rolling-window / buffer
    invariants a streaming adapter only hits after minutes of audio. A
    chapter is one speaker reading continuously, so concatenating its
    utterances in order (utterance number == reading order) reproduces
    that: real long-form speech with an exact reference transcript, rather
    than one clip repeated or synthetic TTS stretched out.

    Picks the chapter with the most utterances in the split (more headroom
    to reach ``minutes``), decodes it in order, and stops as soon as the
    accumulated audio reaches the target — never mid-utterance, so the
    transcript is never truncated mid-word. A chapter shorter than the
    target is used in full (warns rather than failing). Writes the WAV into
    ``out_dir/audio`` and returns the manifest entry — callers own the
    manifest (and NOTICE) file itself, so this drops into either a
    standalone tier or as one more entry alongside the per-utterance ones.
    """
    prefix = f"LibriSpeech/{subset}/"
    target_seconds = minutes * 60

    text: dict[str, str] = {}
    by_chapter: dict[str, list[str]] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(prefix)):
                continue
            if name.endswith(".trans.txt"):
                for line in tar.extractfile(member).read().decode().splitlines():
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac"):
                utt_id = Path(name).stem
                by_chapter.setdefault(_chapter(utt_id), []).append(utt_id)

    chapter_id, utt_ids = max(by_chapter.items(), key=lambda kv: len(kv[1]))
    utt_ids = sorted(utt_ids)
    print(f"longest chapter: {chapter_id} ({len(utt_ids)} utterances)")

    pcm_by_id: dict[str, array] = {}
    with tarfile.open(tar_path, "r:gz") as tar:
        wanted = set(utt_ids)
        for member in tar:
            if not (member.isfile() and member.name.endswith(".flac")):
                continue
            utt_id = Path(member.name).stem
            if utt_id in wanted:
                pcm_by_id[utt_id] = decode_flac(tar.extractfile(member).read())
                wanted.discard(utt_id)
                if not wanted:
                    break

    gap = array("h", bytes(2 * int(LONG_FORM_GAP_SECONDS * RATE)))
    samples = array("h")
    texts: list[str] = []
    used = 0
    for utt_id in utt_ids:
        if utt_id not in pcm_by_id or utt_id not in text:
            continue
        if samples:
            samples.extend(gap)
        samples.extend(pcm_by_id[utt_id])
        texts.append(text[utt_id])
        used += 1
        if len(samples) / RATE >= target_seconds:
            break
    if not samples:
        raise SystemExit(f"chapter {chapter_id} yielded no usable audio")
    if len(samples) / RATE < target_seconds:
        print(
            f"warning: chapter {chapter_id} only has {len(samples) / RATE:.1f}s "
            f"({used} utterances) — short of the {target_seconds:.0f}s target"
        )

    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    clip_id = f"librispeech-{chapter_id}-longform"
    duration = write_wav(audio_dir / f"{clip_id}.wav", samples, RATE)
    print(f"  {clip_id:<30} long-form {duration:6.2f}s  ({used} utterances)")

    return {
        "id": clip_id,
        "path": f"audio/{clip_id}.wav",
        "text": " ".join(texts),
        "language": "en",
        "category": "long-form",
        "duration_seconds": round(duration, 3),
        "sample_rate_hz": RATE,
        "channels": 1,
        "source": (
            f"librispeech:{subset}:{chapter_id} "
            f"({used} utterances concatenated, {LONG_FORM_GAP_SECONDS}s silence gap)"
        ),
        "license": LICENSE,
    }


def build(
    out_dir: Path,
    tar_path: Path,
    n: int,
    *,
    subset: str = "dev-clean",
    select: str = "archive",
    manifest_name: str = "manifest.json",
    long_form_minutes: float | None = None,
) -> Path:
    prefix = f"LibriSpeech/{subset}/"
    # UD129 category: the "-other" splits are LibriSpeech's harder half —
    # accented and lower-fidelity recordings — so they land in "accent", not
    # "quiet". Keeps `bench.py --category` honest across tiers.
    category = "accent" if subset.endswith("-other") else "quiet"
    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    clips = (
        collect_balanced(tar_path, n, prefix)
        if select == "balanced"
        else collect(tar_path, n, prefix)
    )
    if not clips and not long_form_minutes:
        raise SystemExit(f"no clips selected — is this the LibriSpeech {subset} tarball?")

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
            category,
            f"librispeech:{subset}:{utt_id}",
        )
    for utt_id, samples, txt in clips[:N_NOISE]:
        add(
            f"librispeech-{utt_id}-noise-snr{int(NOISE_SNR_DB)}",
            mix_noise(samples, NOISE_SNR_DB, NOISE_SEED),
            txt,
            "noise",
            f"librispeech:{subset}:{utt_id}+noise",
        )

    if long_form_minutes:
        entries.append(long_form_entry(out_dir, tar_path, long_form_minutes, subset))

    # One split per output dir: the NOTICE carries the split's provenance, so
    # mixing splits would silently overwrite one tier's attribution.
    notice_path = out_dir / "NOTICE"
    if notice_path.exists() and f"corpus ({subset})" not in notice_path.read_text(encoding="utf-8"):
        raise SystemExit(
            f"{out_dir} already holds a different LibriSpeech split "
            f"(see its NOTICE) — pass --out for a separate {subset} tier"
        )
    notice_path.write_text(notice_for(subset), encoding="utf-8")
    manifest = out_dir / manifest_name
    manifest.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generator": "dev/fetch_english_corpus.py",
                "generated": {
                    "dataset": "librispeech",
                    "subset": subset,
                    "select": select,
                    "n": n,
                    "noise_snr_db": NOISE_SNR_DB,
                    "noise_seed": NOISE_SEED,
                    "long_form_minutes": long_form_minutes,
                },
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"corpus id {stamp_corpus(manifest)}")
    return manifest


def is_complete(out_dir: Path, manifest_name: str, n: int, subset: str) -> bool:
    """True when ``out_dir`` already holds exactly the corpus these args build.

    Lets a caller that only needs *a* corpus (CI, which restores one from a
    cache) skip the ~330 MB download. Deliberately strict: a manifest from a
    different split or clip count is not the corpus that was asked for, and a
    missing WAV means a half-written tier, so both re-generate.
    """
    manifest = out_dir / manifest_name
    if not manifest.is_file():
        return False
    try:
        clips = json.loads(manifest.read_text(encoding="utf-8"))["clips"]
    except (ValueError, KeyError):
        return False
    if len(clips) != n + N_NOISE:
        return False
    return all(
        clip.get("source", "").startswith(f"librispeech:{subset}:")
        and (out_dir / clip["path"]).is_file()
        for clip in clips
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "corpus" / "english")
    parser.add_argument("--cache", type=Path, default=REPO_ROOT / ".cache" / "librispeech")
    parser.add_argument(
        "--tarball",
        type=Path,
        default=None,
        help="use an already-downloaded <subset>.tar.gz instead of fetching",
    )
    parser.add_argument(
        "--subset",
        choices=SUBSETS,
        default="dev-clean",
        help="LibriSpeech split to draw from (default dev-clean). The '-other'"
        " splits are the harder, accented/low-fidelity half — give them their"
        " own --out, one split per corpus dir",
    )
    parser.add_argument("-n", type=int, default=12, help="number of clean clips (default 12)")
    parser.add_argument(
        "--select",
        choices=("archive", "balanced"),
        default="archive",
        help=(
            "clip selection: 'archive' = first N in archive order (one speaker,"
            " reproduces the original manifest.json); 'balanced' = round-robin"
            " over every speaker in the split (use this for accuracy benchmarks)"
        ),
    )
    parser.add_argument(
        "--manifest-name",
        default="manifest.json",
        help="manifest filename inside --out (default manifest.json); use a"
        " distinct name to add a tier alongside an existing one",
    )
    parser.add_argument(
        "--long-form-minutes",
        type=float,
        default=None,
        help="in addition to the -n per-utterance clips, concatenate one"
        " whole LibriSpeech chapter (in reading order) into a single"
        " continuous clip at least this many minutes long, category"
        " 'long-form' — for rolling-window / buffer invariants that only"
        " show up minutes into a session. Pass -n 0 for a manifest holding"
        " only the long-form clip.",
    )
    parser.add_argument(
        "--skip-complete",
        action="store_true",
        help="exit 0 without downloading when --out already holds exactly this"
        " corpus (same split, clip count, and every WAV present); for CI, which"
        " restores the tier from a cache",
    )
    args = parser.parse_args()
    manifest_name = args.manifest_name

    if (
        not args.long_form_minutes
        and args.skip_complete
        and is_complete(args.out, manifest_name, args.n, args.subset)
    ):
        manifest_path = args.out / manifest_name
        # Stamp even on the skip path: a tier restored from a cache, or built
        # before ids existed, still has to say which corpus it is.
        print(
            f"{args.out} already holds this corpus (id {stamp_corpus(manifest_path)});"
            " skipping fetch"
        )
        return 0

    tar_path = args.tarball or download(
        f"{BASE_URL}/{args.subset}.tar.gz", args.cache / f"{args.subset}.tar.gz"
    )
    manifest = build(
        args.out,
        tar_path,
        args.n,
        subset=args.subset,
        select=args.select,
        manifest_name=manifest_name,
        long_form_minutes=args.long_form_minutes,
    )
    print(f"\nwrote {manifest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
