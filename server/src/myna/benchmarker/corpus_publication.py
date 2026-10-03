"""Full published test splits: ``myna-bench download-corpus --preset <name>``.

    myna-bench download-corpus --preset librispeech-test-clean
    myna-bench download-corpus --preset librispeech-test-other
    myna-bench download-corpus --preset fleurs-test:fr_fr
    myna-bench download-corpus --preset fleurs-smoke

``fleurs-smoke`` is the exception, and no publication number: a few clips of
each language Myna's backends claim, in one manifest, for a nightly accuracy
gate that has to fail on a broken language rather than measure one.

The other corpus tiers are subsets picked for a quick, representative sweep.
A number meant for a paper or the Open ASR Leaderboard is quoted on a whole
test split, so each preset takes every utterance the split has: no selection,
no noise or long-form variants, nothing the reader would have to reproduce.

Archives are cached under the shared ``~/.cache/myna/corpus-src`` (the same
layout the FLEURS subset was built from), so every checkout and worktree reuses
one download. FLEURS is fetched at a pinned dataset revision rather than
``main``. A cache is reused on a size match alone, so neither the URL nor the
revision proves which bytes were read: the manifest records each archive's
sha256 (for FLEURS, comparable with the dataset's LFS oid at that revision),
and the corpus id fingerprints the decoded clips that were scored.

Each manifest records its preset, dataset, split, licence and source URL
beside the clips, and is stamped with the usual ``corpus_id``. Re-running a
preset over a corpus that still verifies is a no-op.

AMI, Earnings-22 and VoxPopuli are not presets yet (their licences are open
questions for the project, not for this tool); TED-LIUM 3 is excluded, its
CC-BY-NC-ND licence does not allow the derived audio this writes.
"""

from __future__ import annotations

import argparse
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from myna.benchmarker import corpus_english
from myna.benchmarker._audio import RATE, write_wav
from myna.benchmarker.corpus_chinese import _clean_reference
from myna.testbed.corpus import sha256_file, stamp_corpus, verify_corpus

PRESETS = ("librispeech-test-clean", "librispeech-test-other", "fleurs-test:<lang>", "fleurs-smoke")

LICENSE = "CC-BY-4.0"
LIBRISPEECH_SPLITS = ("test-clean", "test-other")
FLEURS_REPO = "google/fleurs"
# The dataset commit the presets were built and checked against (2026-09-30).
FLEURS_REVISION = "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
_FLEURS_LOCALE = re.compile(r"[a-z]{2,3}(?:_[a-z0-9]+)+")
# FLEURS locales whose language subtag is not the BCP-47 tag backends are
# sent. Only that: a backend's own code for a language (Whisper's "jw" for
# "jv") is its adapter's business.
_LANGUAGE = {"cmn": "zh"}
# The smoke tier: the first clips, by filename, of each locale.
SMOKE_LOCALES = (
    "en_us",
    "de_de",
    "fr_fr",
    "es_419",
    "it_it",
    "ru_ru",
    "cmn_hans_cn",
    "ja_jp",
    "ko_kr",
)
SMOKE_CLIPS = 10
# A whole split decodes for a minute or more; say so now and then.
PROGRESS_EVERY = 250


@dataclass(frozen=True)
class Preset:
    name: str
    dataset: str
    split: str
    locale: str | None

    @property
    def slug(self) -> str:
        return self.name.replace(":", "-")


def parse_preset(name: str) -> Preset:
    for split in LIBRISPEECH_SPLITS:
        if name == f"librispeech-{split}":
            return Preset(name, "librispeech", split, None)
    if name == "fleurs-smoke":
        return Preset(name, "fleurs-smoke", "test", None)
    prefix, _, locale = name.partition(":")
    if prefix == "fleurs-test" and _FLEURS_LOCALE.fullmatch(locale):
        return Preset(name, "fleurs", "test", locale)
    raise SystemExit(
        f"unknown preset {name!r}; choose one of: {', '.join(PRESETS)}"
        " (<lang> is a FLEURS locale such as fr_fr or cmn_hans_cn)"
    )


def default_cache() -> Path:
    base = os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache"
    return Path(base) / "myna" / "corpus-src"


def librispeech_url(split: str) -> str:
    return f"{corpus_english.BASE_URL}/{split}.tar.gz"


def fleurs_url(locale: str, name: str) -> str:
    return (
        f"https://huggingface.co/datasets/{FLEURS_REPO}/resolve/{FLEURS_REVISION}"
        f"/data/{locale}/{name}"
    )


def _entry(
    clip_id: str, text: str, language: str, category: str, duration: float, source: str
) -> dict[str, object]:
    return {
        "id": clip_id,
        "path": f"audio/{clip_id}.wav",
        "text": text,
        "language": language,
        "category": category,
        "duration_seconds": round(duration, 3),
        "sample_rate_hz": RATE,
        "channels": 1,
        "source": source,
        "license": LICENSE,
    }


def _write(
    out: Path,
    manifest_name: str,
    header: dict[str, Any],
    entries: list[dict[str, object]],
    notice: str,
) -> Path:
    entries.sort(key=lambda e: str(e["id"]))
    (out / "NOTICE").write_text(notice, encoding="utf-8")
    manifest = out / manifest_name
    manifest.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generator": f"myna-bench download-corpus --preset {header['preset']}",
                **header,
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    hours = sum(float(str(e["duration_seconds"])) for e in entries) / 3600
    print(f"{len(entries)} clips, {hours:.2f} h of audio")
    print(f"corpus id {stamp_corpus(manifest)}")
    return manifest


def build_librispeech(
    out: Path, tar_path: Path, split: str, *, manifest_name: str = "manifest.json"
) -> Path:
    """Every utterance of one LibriSpeech split, decoded as it streams past.

    One pass, one clip in memory at a time: a whole split is ~5 h of audio,
    and holding it decoded (as the subset builders do) is ~600 MB.
    """
    prefix = f"LibriSpeech/{split}/"
    category = "accent" if split.endswith("-other") else "quiet"
    audio_dir = out / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)

    durations: dict[str, float] = {}
    text: dict[str, str] = {}
    with corpus_english.open_split(tar_path) as tar:
        for member in tar:
            name = member.name
            if not (member.isfile() and name.startswith(prefix)):
                continue
            stream = tar.extractfile(member)
            assert stream is not None
            if name.endswith(".trans.txt"):
                for line in stream.read().decode().splitlines():
                    utt_id, _, transcript = line.partition(" ")
                    text[utt_id] = transcript
            elif name.endswith(".flac"):
                utt_id = Path(name).stem
                samples = corpus_english.decode_audio(stream.read())
                durations[utt_id] = write_wav(audio_dir / f"librispeech-{utt_id}.wav", samples)
                if len(durations) % PROGRESS_EVERY == 0:
                    print(f"  {len(durations)} utterances decoded...")
    if not durations:
        raise SystemExit(f"no utterances under {prefix} in {tar_path}")

    untranscribed = [u for u in durations if u not in text]
    for utt_id in untranscribed:
        (audio_dir / f"librispeech-{utt_id}.wav").unlink()
    if untranscribed:
        print(f"dropped {len(untranscribed)} utterance(s) without a transcript")

    entries = [
        _entry(
            f"librispeech-{utt_id}",
            text[utt_id],
            "en",
            category,
            duration,
            f"librispeech:{split}:{utt_id}",
        )
        for utt_id, duration in durations.items()
        if utt_id in text
    ]
    header = {
        "preset": f"librispeech-{split}",
        "dataset": "librispeech",
        "split": split,
        "language": "en",
        "license": LICENSE,
        "source": librispeech_url(split),
        "source_sha256": {f"{split}.tar.gz": sha256_file(tar_path)},
        "citation": (
            "V. Panayotov, G. Chen, D. Povey, S. Khudanpur, "
            '"Librispeech: an ASR corpus based on public domain audio books", ICASSP 2015'
        ),
    }
    return _write(out, manifest_name, header, entries, corpus_english.notice_for(split))


def _fleurs_entries(
    out: Path,
    tsv_path: Path,
    tar_path: Path,
    locale: str,
    *,
    limit: int | None = None,
    category: str | None = None,
) -> list[dict[str, object]]:
    """The listed clips of one FLEURS locale's archive, decoded into ``out``:
    every one, or the first ``limit`` by filename."""
    subtag = locale.split("_", 1)[0]
    language = _LANGUAGE.get(subtag, subtag)
    category = category or ("quiet" if language == "en" else "non-english")

    # id, filename, raw_transcription, transcription, chars, num_samples, gender.
    # Split on tabs, not with csv: the raw text carries literal quote marks.
    reference: dict[str, str] = {}
    for line in tsv_path.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) >= 3 and fields[2].strip():
            reference[fields[1]] = _clean_reference(fields[2])
    if limit is not None:
        reference = {name: reference[name] for name in sorted(reference)[:limit]}

    audio_dir = out / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    entries: list[dict[str, object]] = []
    with corpus_english.open_split(tar_path) as tar:
        for member in tar:
            filename = Path(member.name).name
            if not (member.isfile() and filename in reference):
                continue
            stream = tar.extractfile(member)
            assert stream is not None
            clip_id = f"fleurs-{locale}-{Path(filename).stem}"
            samples = corpus_english.decode_audio(stream.read())
            duration = write_wav(audio_dir / f"{clip_id}.wav", samples)
            entries.append(
                _entry(
                    clip_id,
                    reference[filename],
                    language,
                    category,
                    duration,
                    f"fleurs:{locale}:test:{filename}",
                )
            )
            if len(entries) % PROGRESS_EVERY == 0:
                print(f"  {len(entries)} clips decoded...")
    if not entries:
        raise SystemExit(f"no clips in {tar_path} are listed in {tsv_path}")
    if missing := len(reference) - len(entries):
        print(f"{missing} listed clip(s) had no audio in the archive")
    return entries


_FLEURS_CITATION = (
    'A. Conneau et al., "FLEURS: Few-shot Learning Evaluation of Universal '
    'Representations of Speech", IEEE SLT 2022'
)


def _fleurs_notice(title: str, preset: str) -> str:
    return (
        f"{title}\n"
        "\n"
        "Derived from Google FLEURS, redistributed under its original licence.\n"
        f"Regenerate with: myna-bench download-corpus --preset {preset}\n"
        "\n"
        f"  Source:  https://huggingface.co/datasets/{FLEURS_REPO} (revision {FLEURS_REVISION})\n"
        f"  Licence: {LICENSE}  (https://creativecommons.org/licenses/by/4.0/)\n"
        '  Cite:    A. Conneau et al., "FLEURS: Few-shot Learning Evaluation of Universal\n'
        '           Representations of Speech", IEEE SLT 2022.\n'
        "\n"
        "Audio is decoded to 16 kHz mono S16LE WAV; references are the raw transcriptions.\n"
    )


def build_fleurs(
    out: Path, tsv_path: Path, tar_path: Path, locale: str, *, manifest_name: str = "manifest.json"
) -> Path:
    """Every clip of one FLEURS locale's test split, scored against its raw
    transcription (normalising is the scorer's job, and both normalisers do it).

    FLEURS reads each sentence with several speakers, so a sentence id repeats
    across rows; the audio filename is what is unique.
    """
    entries = _fleurs_entries(out, tsv_path, tar_path, locale)
    header = {
        "preset": f"fleurs-test:{locale}",
        "dataset": "fleurs",
        "split": "test",
        "language": locale,
        "license": LICENSE,
        "source": fleurs_url(locale, "audio/test.tar.gz"),
        "source_sha256": {
            "test.tsv": sha256_file(tsv_path),
            "audio/test.tar.gz": sha256_file(tar_path),
        },
        "reference": "raw_transcription",
        "citation": _FLEURS_CITATION,
    }
    title = f"Publication corpus: FLEURS {locale} test split, every clip"
    return _write(
        out, manifest_name, header, entries, _fleurs_notice(title, f"fleurs-test:{locale}")
    )


def build_fleurs_smoke(
    out: Path, sources: dict[str, tuple[Path, Path]], *, manifest_name: str = "manifest.json"
) -> Path:
    """The first ``SMOKE_CLIPS`` clips of each locale in ``sources``
    ({locale: (test.tsv, test.tar.gz)}), filed under their locale."""
    entries: list[dict[str, object]] = []
    digests: dict[str, str] = {}
    for locale, (tsv_path, tar_path) in sources.items():
        entries += _fleurs_entries(
            out, tsv_path, tar_path, locale, limit=SMOKE_CLIPS, category=locale
        )
        digests[f"{locale}/test.tsv"] = sha256_file(tsv_path)
        digests[f"{locale}/audio/test.tar.gz"] = sha256_file(tar_path)
    header = {
        "preset": "fleurs-smoke",
        "dataset": "fleurs",
        "split": "test",
        "language": "multilingual",
        "license": LICENSE,
        "source": f"https://huggingface.co/datasets/{FLEURS_REPO}",
        "source_sha256": digests,
        "reference": "raw_transcription",
        "citation": _FLEURS_CITATION,
    }
    title = f"Smoke corpus: the first {SMOKE_CLIPS} FLEURS test clips of {', '.join(sources)}"
    return _write(out, manifest_name, header, entries, _fleurs_notice(title, "fleurs-smoke"))


_SUBSET_FLAGS = (
    ("subset", "--subset"),
    ("n", "-n"),
    ("select", "--select"),
    ("tarball", "--tarball"),
    ("long_form_minutes", "--long-form-minutes"),
    ("skip_complete", "--skip-complete"),
)


def _built(manifest_path: Path, preset: Preset) -> bool:
    """True when ``manifest_path`` is this preset's corpus and still verifies.

    Refuses a manifest from anything else: overwriting it would leave its
    results naming a corpus that no longer exists.
    """
    if not manifest_path.is_file():
        return False
    try:
        held = json.loads(manifest_path.read_text(encoding="utf-8")).get("preset")
    except ValueError:
        held = None
    if held != preset.name:
        raise SystemExit(
            f"{manifest_path} holds {held or 'a corpus no preset built'}, not {preset.name}"
            " - pass --out for a separate directory"
        )
    try:
        identity = verify_corpus(manifest_path)
    except ValueError as exc:
        print(f"rebuilding: {exc}")
        return False
    print(f"{manifest_path.parent} already holds {preset.name} (id {identity}); nothing to do")
    return True


def _fetch_fleurs(cache: Path, locale: str) -> tuple[Path, Path]:
    """One locale's test.tsv and audio archive, downloaded once into ``cache``."""
    where = cache / "fleurs" / locale
    tsv_path = corpus_english.download(fleurs_url(locale, "test.tsv"), where / "test.tsv")
    tar_path = corpus_english.download(
        fleurs_url(locale, "audio/test.tar.gz"), where / "test.tar.gz"
    )
    return tsv_path, tar_path


def cmd_preset(args: argparse.Namespace) -> None:
    """``download-corpus --preset``: fetch (or reuse) a whole split, write its manifest."""
    preset = parse_preset(args.preset)
    given = [
        flag
        for attr, flag in _SUBSET_FLAGS
        if (value := getattr(args, attr)) is not None and value is not False
    ]
    if given:
        raise SystemExit(
            f"--preset takes the whole split; drop {', '.join(given)}"
            " (they select a subset, which a publication number must not)"
        )
    out = Path(args.out or Path("corpus") / "publication" / preset.slug)
    cache = Path(args.cache) if args.cache else default_cache()
    manifest_path = out / args.manifest_name
    if _built(manifest_path, preset):
        return

    corpus_english.require_ffmpeg()
    if preset.dataset == "librispeech":
        tar_path = corpus_english.download(
            librispeech_url(preset.split), cache / "librispeech" / f"{preset.split}.tar.gz"
        )
        manifest = build_librispeech(out, tar_path, preset.split, manifest_name=args.manifest_name)
    elif preset.locale is None:
        sources = {locale: _fetch_fleurs(cache, locale) for locale in SMOKE_LOCALES}
        manifest = build_fleurs_smoke(out, sources, manifest_name=args.manifest_name)
    else:
        tsv_path, tar_path = _fetch_fleurs(cache, preset.locale)
        manifest = build_fleurs(
            out, tsv_path, tar_path, preset.locale, manifest_name=args.manifest_name
        )
    print(f"\nwrote {manifest}")
    print(f"Sweep it:  myna-bench run --config <config> --manifest {manifest}")
