"""The vendored text-only evaluation corpus (``myna-bench text``).

Three JSONL slices under ``textcorpus/``, pinned by ``SHA256SUMS``:

``clean_en``        200 punctuated, cased FLEURS ``en_us`` test sentences that
                    a cleanup step must leave byte-identical.
``disfluent_en``    (heard, intended, category) pairs authored for this repo.
``translate_en_fr`` 200 FLEURS sentences aligned by FLEURS sentence id,
                    ``en_us`` -> ``fr_fr``.

``NOTICE`` carries the provenance. The FLEURS slices are regenerated with
``myna-bench build-text-corpus`` from the cached FLEURS ``test.tsv`` files; the
selection is a seeded hash order, so the output is reproducible byte for byte.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from dataclasses import dataclass
from pathlib import Path

CORPUS_DIR = Path(__file__).parent / "textcorpus"
SUMS = "SHA256SUMS"
SLICES = ("clean_en", "disfluent_en", "translate_en_fr")
SEED = "myna-text-corpus-v1"
N_CLEAN = 200
N_TRANSLATE = 200
# A translation latency probe wants real sentences, not fragments.
MIN_WORDS, MAX_WORDS = 6, 40


class CorpusError(Exception):
    """The corpus on disk is not the pinned one."""


@dataclass(frozen=True)
class Pair:
    id: str
    heard: str
    intended: str
    category: str


@dataclass(frozen=True)
class Translation:
    id: str
    en: str
    fr: str


@dataclass(frozen=True)
class Clean:
    id: str
    text: str


@dataclass(frozen=True)
class TextCorpus:
    clean_en: tuple[Clean, ...]
    disfluent_en: tuple[Pair, ...]
    translate_en_fr: tuple[Translation, ...]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_sums(directory: Path) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in (directory / SUMS).read_text(encoding="utf-8").splitlines():
        digest, _, name = line.partition("  ")
        sums[name] = digest
    return sums


def write_sums(directory: Path) -> None:
    lines = [f"{sha256_file(directory / f'{name}.jsonl')}  {name}.jsonl\n" for name in SLICES]
    (directory / SUMS).write_text("".join(lines), encoding="utf-8")


def verify(directory: Path = CORPUS_DIR) -> None:
    sums = read_sums(directory)
    for name in SLICES:
        file = f"{name}.jsonl"
        if sums.get(file) != sha256_file(directory / file):
            raise CorpusError(f"{file} does not match its pin in {SUMS}")


def _rows(directory: Path, name: str) -> list[dict[str, str]]:
    text = (directory / f"{name}.jsonl").read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def load(directory: Path = CORPUS_DIR, *, check: bool = True) -> TextCorpus:
    """The corpus, after verifying every slice against ``SHA256SUMS``."""
    if check:
        verify(directory)
    return TextCorpus(
        clean_en=tuple(Clean(r["id"], r["text"]) for r in _rows(directory, "clean_en")),
        disfluent_en=tuple(
            Pair(r["id"], r["heard"], r["intended"], r["category"])
            for r in _rows(directory, "disfluent_en")
        ),
        translate_en_fr=tuple(
            Translation(r["id"], r["en"], r["fr"]) for r in _rows(directory, "translate_en_fr")
        ),
    )


def read_fleurs_tsv(path: Path) -> dict[str, str]:
    """FLEURS sentence id -> raw (cased, punctuated) transcription.

    Columns are id, filename, raw transcription, normalised transcription,
    characters, samples, gender; one row per recording, so an id repeats for
    each speaker and every repeat must carry the same text. Tab-split rather
    than csv: the raw text carries literal quote marks."""
    sentences: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) < 3 or not fields[2].strip():
            continue
        text = " ".join(fields[2].split())
        if sentences.setdefault(fields[0], text) != text:
            raise CorpusError(f"{path}: sentence {fields[0]} has two different transcripts")
    return sentences


def _order(slice_name: str, ids: set[str]) -> list[str]:
    def key(sentence_id: str) -> str:
        return hashlib.sha256(f"{SEED}:{slice_name}:{sentence_id}".encode()).hexdigest()

    return sorted(ids, key=key)


def _is_clean_sentence(text: str) -> bool:
    count = len(text.split())
    return text[:1].isupper() and text[-1:] in ".?!" and MIN_WORDS <= count <= MAX_WORDS


def select_fleurs(
    english: dict[str, str], french: dict[str, str]
) -> tuple[list[Clean], list[Translation]]:
    clean_ids = _order("clean_en", {i for i, t in english.items() if _is_clean_sentence(t)})
    aligned = {
        i
        for i in english.keys() & french.keys()
        if _is_clean_sentence(english[i]) and french[i][-1:] in ".?!»"
    }
    translate_ids = _order("translate_en_fr", aligned)
    if len(clean_ids) < N_CLEAN or len(translate_ids) < N_TRANSLATE:
        raise CorpusError("FLEURS has too few usable sentences for the corpus")
    clean = [Clean(f"fleurs-en_us-{i}", english[i]) for i in sorted(clean_ids[:N_CLEAN], key=int)]
    translate = [
        Translation(f"fleurs-{i}", english[i], french[i])
        for i in sorted(translate_ids[:N_TRANSLATE], key=int)
    ]
    return clean, translate


def _dump(rows: list[dict[str, str]]) -> str:
    return "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows)


def cmd_build(args: argparse.Namespace) -> None:
    """``myna-bench build-text-corpus``: rewrite the two FLEURS slices and the pins."""
    fleurs = Path(args.fleurs)
    out = Path(args.out)
    english = read_fleurs_tsv(fleurs / "en_us" / "test.tsv")
    french = read_fleurs_tsv(fleurs / "fr_fr" / "test.tsv")
    clean, translate = select_fleurs(english, french)
    (out / "clean_en.jsonl").write_text(
        _dump([{"id": c.id, "text": c.text} for c in clean]), encoding="utf-8"
    )
    (out / "translate_en_fr.jsonl").write_text(
        _dump([{"id": t.id, "en": t.en, "fr": t.fr} for t in translate]), encoding="utf-8"
    )
    write_sums(out)
    print(f"wrote {len(clean)} clean_en and {len(translate)} translate_en_fr rows to {out}")
