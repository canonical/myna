"""Build the English recorded-speech corpus tier from LibriSpeech.

    myna-bench download-corpus [--out corpus/english] [-n 12] [--select balanced]

Real English human speech with exact reference transcripts, so WER is
trustworthy - the synthetic espeak tier is out-of-distribution and its WER is
misleading across architectures (Nemotron ~0% on real voice vs ~45% on espeak).

Source: LibriSpeech (Panayotov et al., ICASSP 2015), CC-BY-4.0, real read
English at 16 kHz. ``--subset`` picks the split: the ``-clean`` ones are
well-recorded speech, the ``-other`` ones LibriSpeech's deliberately harder half
(accented, noisier, lower-fidelity) - the pair papers quote WER on. One split
per output dir, so an ``-other`` tier needs its own ``--out``. Each ~330 MB
download is cached; corpora are regenerated on demand, not committed.

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
per-speaker WER can be broken out without extra manifest fields.

Requires ffmpeg (FLAC decode). Network is needed only for the download.
"""

from __future__ import annotations

import contextlib
import gzip
import json
import subprocess
import tarfile
import urllib.request
import zlib
from array import array
from pathlib import Path

from myna.benchmarker._audio import NOISE_SEED, NOISE_SNR_DB, mix_noise, write_wav
from myna.testbed.corpus import stamp_corpus

RATE = 16_000
BASE_URL = "https://www.openslr.org/resources/12"
# The LibriSpeech splits worth sweeping. "clean" is well-recorded read speech;
# "other" is the deliberately harder half (accented, noisier, lower-fidelity
# recordings) - the pair papers report WER on, so numbers here are comparable
# to published figures.
SUBSETS = ("dev-clean", "dev-other", "test-clean", "test-other")
LICENSE = "CC-BY-4.0"
N_NOISE = 2


def notice_for(subset: str) -> str:
    return f"""\
Real recorded-speech corpus tier (T25)

Derived from the LibriSpeech ASR corpus ({subset}), redistributed under its
original licence. Regenerate with: myna-bench download-corpus

  Source:  {BASE_URL}/{subset}.tar.gz
  Licence: CC-BY-4.0  (https://creativecommons.org/licenses/by/4.0/)
  Cite:    V. Panayotov, G. Chen, D. Povey, S. Khudanpur, "Librispeech: an ASR
           corpus based on public domain audio books", ICASSP 2015.

Each clip's source utterance id and licence are in the manifest. Audio is
decoded to 16 kHz mono S16LE WAV; "noise" clips add seeded Gaussian noise at
{NOISE_SNR_DB:.0f} dB SNR.
"""


def _remote_size(url: str) -> int | None:
    """Content-Length for ``url``, or None when it cannot be asked."""
    request = urllib.request.Request(url, method="HEAD")  # noqa: S310
    try:
        with urllib.request.urlopen(request, timeout=30) as resp:  # noqa: S310
            return int(resp.headers.get("Content-Length") or 0) or None
    except (OSError, ValueError):
        return None


def download(url: str, dest: Path) -> Path:
    """Fetch a split tarball, or reuse a *complete* cached one.

    Two things here are not decoration. The download goes to a ``.part`` file
    and is renamed only once it finishes, so an interrupted one can never be
    mistaken for a cache hit; and a pre-existing cache is checked against the
    server's Content-Length before it is trusted. Without either, a download
    killed at 7% left a 25 MB stub that every later run reported as "using
    cached" and then failed on deep inside tarfile, with a gzip EOFError that
    names neither the file nor the cause.

    The progress line matters more than it looks too: this is a 330 MB download
    in a tool someone runs once, and a silent five-minute pause reads as a hang.
    """
    dest.parent.mkdir(parents=True, exist_ok=True)
    expected = _remote_size(url)
    if dest.exists() and dest.stat().st_size:
        have = dest.stat().st_size
        if expected is None:
            # Offline, or a server that will not answer a HEAD. Use the cache
            # and let the archive reader report it if it is short.
            print(f"using cached {dest} ({have >> 20} MB, unverified - server did not answer)")
            return dest
        if have == expected:
            print(f"using cached {dest} ({have >> 20} MB)")
            return dest
        print(
            f"cached {dest} is {have >> 20} MB, expected {expected >> 20} MB "
            "- refetching (an earlier download did not finish)"
        )

    part = dest.with_suffix(dest.suffix + ".part")
    print(f"downloading {url}  ({(expected or 0) >> 20 or '~330'} MB)\n  -> {dest}")
    try:
        with urllib.request.urlopen(url) as resp, part.open("wb") as out:  # noqa: S310
            total = int(resp.headers.get("Content-Length") or 0)
            received = 0
            while block := resp.read(1 << 20):
                out.write(block)
                received += len(block)
                if total:
                    pct = received / total * 100
                    print(
                        f"  {pct:5.1f}%  {received >> 20} / {total >> 20} MB\r",
                        end="",
                        flush=True,
                    )
        print()
        if total and received != total:
            raise OSError(f"got {received} bytes, expected {total}")
    except BaseException:
        # Includes KeyboardInterrupt: a half-written .part left behind is the
        # whole failure mode this function exists to prevent.
        part.unlink(missing_ok=True)
        raise
    part.replace(dest)
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


def open_split(tar_path: Path):
    """Open a split tarball, turning a short or corrupt one into advice.

    A truncated archive fails deep inside tarfile with a gzip EOFError that
    names neither the file nor the cause. This is reachable whenever the cache
    predates the atomic download below, or the disk filled mid-write.
    """

    @contextlib.contextmanager
    def _reader():
        try:
            with tarfile.open(tar_path, "r:gz") as tar:
                yield tar
        except (EOFError, tarfile.ReadError, gzip.BadGzipFile, zlib.error) as exc:
            size = tar_path.stat().st_size if tar_path.exists() else 0
            raise SystemExit(
                f"{tar_path} is not a complete LibriSpeech archive ({size >> 20} MB): {exc}. "
                f"Delete it and re-run - the download will restart:\n  rm {tar_path}"
            ) from exc

    return _reader()


def collect(tar_path: Path, n: int, prefix: str) -> list[tuple[str, array, str]]:
    """The first ``n`` utterances in archive order, with their transcripts."""
    pcm: dict[str, array] = {}
    text: dict[str, str] = {}
    with open_split(tar_path) as tar:
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
    with open_split(tar_path) as tar:
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
    with open_split(tar_path) as tar:
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
    with open_split(tar_path) as tar:
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
    with open_split(tar_path) as tar:
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
                "generator": "myna-bench download-corpus",
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


def require_ffmpeg() -> None:
    """Refuse before the 330 MB download, not after it.

    Every clip is decoded from FLAC through ffmpeg, so a machine without it
    cannot build a corpus at all - and finding that out on the first clip means
    the download was wasted.
    """
    try:
        subprocess.run(["ffmpeg", "-version"], capture_output=True, check=True)
    except (OSError, subprocess.SubprocessError) as err:
        raise SystemExit("ffmpeg is required for FLAC decode: sudo apt install ffmpeg") from err


def cmd_download(args) -> None:  # noqa: ANN001
    """``download-corpus``: fetch (or reuse) a split and write its manifest."""
    out = Path(args.out)
    manifest_name = args.manifest_name

    if (
        not args.long_form_minutes
        and args.skip_complete
        and is_complete(out, manifest_name, args.n, args.subset)
    ):
        manifest_path = out / manifest_name
        # Stamp even on the skip path: a tier restored from a cache, or built
        # before ids existed, still has to say which corpus it is.
        print(f"{out} already holds this corpus (id {stamp_corpus(manifest_path)}); skipping fetch")
        return

    require_ffmpeg()
    tar_path = (
        Path(args.tarball)
        if args.tarball
        else download(
            f"{BASE_URL}/{args.subset}.tar.gz",
            Path(args.cache) / f"{args.subset}.tar.gz",
        )
    )
    manifest = build(
        out,
        tar_path,
        args.n,
        subset=args.subset,
        select=args.select,
        manifest_name=manifest_name,
        long_form_minutes=args.long_form_minutes,
    )
    print(f"\nwrote {manifest}")
    print(f"Use in bench.yaml:  manifest: {manifest}")
