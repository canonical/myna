"""The vendored text-only evaluation corpus (``myna-bench text``).

Three JSONL slices under ``textcorpus/``, pinned by ``SHA256SUMS``:

``clean_en``        200 punctuated, cased FLEURS ``en_us`` test sentences that
                    a cleanup step must leave byte-identical.
``disfluent_en``    (heard, intended, category) pairs authored for this repo.
``translate_en_fr`` 200 FLEURS sentences aligned by FLEURS sentence id,
                    ``en_us`` -> ``fr_fr``.
``translate_en_<l>`` the same for any other FLEURS language ``l`` (a Mozilla
                    code, see ``fleurs_codes``), built by
                    ``myna-bench build-text-corpus --pair en:<l>``.

``NOTICE`` carries the provenance. The FLEURS slices are regenerated with
``myna-bench build-text-corpus`` from the cached FLEURS ``test.tsv`` files; the
selection is a seeded hash order, so the output is reproducible byte for byte.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from myna.benchmarker import fleurs_codes

CORPUS_DIR = Path(__file__).parent / "textcorpus"
SUMS = "SHA256SUMS"
SLICES = ("clean_en", "disfluent_en", "translate_en_fr")
SEED = "myna-text-corpus-v1"
N_CLEAN = 200
N_TRANSLATE = 200
PAIR_RE = re.compile(r"translate_en_([A-Za-z]+(?:-[A-Za-z]+)*)")
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
    target: str


@dataclass(frozen=True)
class Clean:
    id: str
    text: str


@dataclass(frozen=True)
class TextCorpus:
    clean_en: tuple[Clean, ...]
    disfluent_en: tuple[Pair, ...]
    pairs: Mapping[str, tuple[Translation, ...]]

    @property
    def translate_en_fr(self) -> tuple[Translation, ...]:
        return self.pairs["fr"]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_sums(directory: Path) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in (directory / SUMS).read_text(encoding="utf-8").splitlines():
        digest, _, name = line.partition("  ")
        sums[name] = digest
    return sums


def pair_slice(lang: str) -> str:
    return f"translate_en_{lang}"


def slice_names(directory: Path = CORPUS_DIR) -> list[str]:
    """The three base slices, then every ``translate_en_<l>`` slice on disk."""
    pairs = sorted(
        p.stem for p in directory.glob("translate_en_*.jsonl") if PAIR_RE.fullmatch(p.stem)
    )
    return [*SLICES[:2], *sorted({"translate_en_fr", *pairs})]


def write_sums(directory: Path) -> None:
    lines = [
        f"{sha256_file(directory / f'{n}.jsonl')}  {n}.jsonl\n" for n in slice_names(directory)
    ]
    (directory / SUMS).write_text("".join(lines), encoding="utf-8")


def verify(directory: Path = CORPUS_DIR) -> None:
    sums = read_sums(directory)
    for name in slice_names(directory):
        file = f"{name}.jsonl"
        if sums.get(file) != sha256_file(directory / file):
            raise CorpusError(f"{file} does not match its pin in {SUMS}")
    for file in sums:
        if not (directory / file).is_file():
            raise CorpusError(f"{file} is pinned in {SUMS} but missing")


def _rows(directory: Path, name: str) -> list[dict[str, str]]:
    text = (directory / f"{name}.jsonl").read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def load_pair(
    lang: str, directory: Path = CORPUS_DIR, *, check: bool = True
) -> tuple[Translation, ...]:
    """One ``translate_en_<lang>`` slice, after verifying the corpus pins."""
    if check:
        verify(directory)
    name = pair_slice(lang)
    if not (directory / f"{name}.jsonl").is_file():
        raise CorpusError(f"no slice {name}: build it with build-text-corpus --pair en:{lang}")
    return tuple(Translation(r["id"], r["en"], r[lang]) for r in _rows(directory, name))


def load(directory: Path = CORPUS_DIR, *, check: bool = True) -> TextCorpus:
    """The corpus, after verifying every slice against ``SHA256SUMS``."""
    if check:
        verify(directory)
    pair_names = {n for n in slice_names(directory) if PAIR_RE.fullmatch(n)}
    langs = [n.removeprefix("translate_en_") for n in slice_names(directory) if n in pair_names]
    return TextCorpus(
        clean_en=tuple(Clean(r["id"], r["text"]) for r in _rows(directory, "clean_en")),
        disfluent_en=tuple(
            Pair(r["id"], r["heard"], r["intended"], r["category"])
            for r in _rows(directory, "disfluent_en")
        ),
        pairs={lang: load_pair(lang, directory, check=False) for lang in langs},
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


def _target_done(text: str) -> bool:
    return bool(text) and not text[-1].isalnum()


def select_pair(english: dict[str, str], target: dict[str, str], lang: str) -> list[Translation]:
    """200 ID-aligned ``en_us`` -> ``lang`` pairs, in FLEURS id order."""
    aligned = {
        i
        for i in english.keys() & target.keys()
        if _is_clean_sentence(english[i]) and _target_done(target[i])
    }
    ids = _order(pair_slice(lang), aligned)
    if len(ids) < N_TRANSLATE:
        raise CorpusError("FLEURS has too few usable sentences for the corpus")
    return [
        Translation(f"fleurs-{i}", english[i], target[i])
        for i in sorted(ids[:N_TRANSLATE], key=int)
    ]


def select_fleurs(
    english: dict[str, str], french: dict[str, str]
) -> tuple[list[Clean], list[Translation]]:
    clean_ids = _order("clean_en", {i for i, t in english.items() if _is_clean_sentence(t)})
    if len(clean_ids) < N_CLEAN:
        raise CorpusError("FLEURS has too few usable sentences for the corpus")
    clean = [Clean(f"fleurs-en_us-{i}", english[i]) for i in sorted(clean_ids[:N_CLEAN], key=int)]
    return clean, select_pair(english, french, "fr")


def _dump(rows: list[dict[str, str]]) -> str:
    return "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows)


NOTICE = "NOTICE"
PAIR_INPUT = "  Pair input: "
PAIR_HEADING = [
    "translate_en_<l>, the other pair slices",
    "  Derived from FLEURS exactly like translate_en_fr above (CC-BY-4.0, same",
    "  revision, same seeded selection and filters); the target-language inputs are:",
]


def _notice_pair(out: Path, lang: str, code: str, digest: str) -> None:
    """Record the target ``test.tsv`` of a pair slice in NOTICE, one line per slice."""
    path = out / NOTICE
    marker = f"{PAIR_INPUT}{pair_slice(lang)} "
    lines = path.read_text(encoding="utf-8").splitlines()
    kept = [ln for ln in lines if not ln.startswith(marker) and ln not in PAIR_HEADING]
    head = [ln for ln in kept if not ln.startswith(PAIR_INPUT)]
    while head and not head[-1]:
        head.pop()
    pairs = [ln for ln in kept if ln.startswith(PAIR_INPUT)]
    pairs.append(f"{marker}data/{code}/test.tsv sha256 {digest}")
    body = [*head, "", *PAIR_HEADING, *sorted(pairs)]
    path.write_text("\n".join(body) + "\n", encoding="utf-8")


def _build_pair(fleurs: Path, out: Path, spec: str) -> None:
    source, _, lang = spec.partition(":")
    if source != "en" or not lang:
        raise CorpusError(f"--pair wants en:<language>, got {spec!r}")
    code = fleurs_codes.fleurs_code(lang)
    english = read_fleurs_tsv(fleurs / "en_us" / "test.tsv")
    target_path = fleurs / code / "test.tsv"
    rows = select_pair(english, read_fleurs_tsv(target_path), lang)
    (out / f"{pair_slice(lang)}.jsonl").write_text(
        _dump([{"id": t.id, "en": t.en, lang: t.target} for t in rows]), encoding="utf-8"
    )
    write_sums(out)
    _notice_pair(out, lang, code, sha256_file(target_path))
    print(f"wrote {len(rows)} {pair_slice(lang)} rows to {out}")


def cmd_build(args: argparse.Namespace) -> None:
    """``myna-bench build-text-corpus``: rewrite the FLEURS slices and the pins.

    With ``--pair en:<l>`` only that pair slice is built."""
    fleurs = Path(args.fleurs)
    out = Path(args.out)
    if getattr(args, "pair", None):
        _build_pair(fleurs, out, args.pair)
        return
    english = read_fleurs_tsv(fleurs / "en_us" / "test.tsv")
    french = read_fleurs_tsv(fleurs / "fr_fr" / "test.tsv")
    clean, translate = select_fleurs(english, french)
    (out / "clean_en.jsonl").write_text(
        _dump([{"id": c.id, "text": c.text} for c in clean]), encoding="utf-8"
    )
    (out / "translate_en_fr.jsonl").write_text(
        _dump([{"id": t.id, "en": t.en, "fr": t.target} for t in translate]), encoding="utf-8"
    )
    write_sums(out)
    print(f"wrote {len(clean)} clean_en and {len(translate)} translate_en_fr rows to {out}")
