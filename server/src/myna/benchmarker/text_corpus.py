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

``eval/run1``, ``eval/run2`` and ``eval/run3`` hold the per-operation evaluation
corpora of ``myna-bench text --eval`` (see ``text_eval``), pinned by
``eval/SHA256SUMS``. Run 3 is the tidy slice written blind for the multilingual
tagger; it grades breaks on run 2's clean sentences.

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
EVAL_DIR = CORPUS_DIR / "eval"
EVAL_RUNS = ("run1", "run2", "run3")
EVAL_LANGS = ("en", "fr", "de", "es")
CLEAN_PER_LANG = 100
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
class EvalItem:
    """One input of the per-operation evaluation and the outputs that count as right.

    ``kind`` is ``positive`` (must change), ``negative`` (must stay) or
    ``clean`` (an ordinary sentence, any change is a break)."""

    id: str
    op: str
    lang: str
    input: str
    refs: tuple[str, ...]
    kind: str
    cls: str


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


def eval_names(run: str, directory: Path = EVAL_DIR) -> list[str]:
    """The pinned files of one evaluation run, relative to ``directory``."""
    if run not in EVAL_RUNS:
        raise CorpusError(f"unknown evaluation run {run!r}: one of {', '.join(EVAL_RUNS)}")
    return sorted(f"{run}/{p.name}" for p in (directory / run).glob("*.jsonl"))


def write_eval_sums(directory: Path = EVAL_DIR) -> None:
    names = [n for run in EVAL_RUNS for n in eval_names(run, directory)]
    lines = [f"{sha256_file(directory / n)}  {n}\n" for n in names]
    (directory / SUMS).write_text("".join(lines), encoding="utf-8")


def verify_eval(directory: Path = EVAL_DIR) -> None:
    sums = read_sums(directory)
    on_disk = {n for run in EVAL_RUNS for n in eval_names(run, directory)}
    for name in sorted(on_disk | set(sums)):
        if name not in sums:
            raise CorpusError(f"eval/{name} is not pinned in eval/{SUMS}")
        if name not in on_disk:
            raise CorpusError(f"eval/{name} is pinned in eval/{SUMS} but missing")
        if sums[name] != sha256_file(directory / name):
            raise CorpusError(f"eval/{name} does not match its pin in eval/{SUMS}")


def _clean_sentences(lang: str, run: str, directory: Path) -> list[str]:
    """The ordinary sentences a run grades breaks on, ``CLEAN_PER_LANG`` per language.

    Run 1 reuses the first lines of the FLEURS test slices; runs 2 and 3 use
    run 2's FLEURS dev sentences that no arm was trained on."""
    if run in ("run2", "run3"):
        rows = _rows(directory / "run2", "clean_dev")
        return [r["text"] for r in rows if r["lang"] == lang][:CLEAN_PER_LANG]
    base = directory.parent
    if lang == "en":
        return [r["text"] for r in _rows(base, "clean_en")][:CLEAN_PER_LANG]
    return [r[lang] for r in _rows(base, pair_slice(lang))][:CLEAN_PER_LANG]


def load_eval(
    run: str,
    directory: Path = EVAL_DIR,
    *,
    check: bool = True,
    skip_inputs: frozenset[str] = frozenset(),
) -> list[EvalItem]:
    """Every item of one evaluation run: the corpus rows, then the clean sentences.

    Each operation in a language gets its own copy of that language's clean
    sentences (ids ``clean-<op>-<lang>-<k>``; ``clean2-`` for run 2), so an
    operation's break rate is measured on its own items. ``skip_inputs`` drops
    corpus rows whose input is a prompt example."""
    if check:
        verify_eval(directory)
        verify(directory.parent)
    prefix = "clean2" if run in ("run2", "run3") else "clean"
    groups: dict[tuple[str, str], list[EvalItem]] = {}
    for name in eval_names(run, directory):
        stem = Path(name).stem
        if stem == "clean_dev":
            continue
        for row in _rows(directory / run, stem):
            if row["input"] in skip_inputs:
                continue
            item = EvalItem(
                row["id"],
                row["op"],
                row["lang"],
                row["input"],
                tuple(row["refs"]),
                row["kind"],
                row["class"],
            )
            groups.setdefault((item.op, item.lang), []).append(item)
    items: list[EvalItem] = []
    for (op, lang), rows in sorted(groups.items()):
        items.extend(rows)
        for k, text in enumerate(_clean_sentences(lang, run, directory)):
            items.append(
                EvalItem(f"{prefix}-{op}-{lang}-{k:03}", op, lang, text, (text,), "clean", "clean")
            )
    return items


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


# Languages whose FLEURS transcripts end a sentence with no punctuation.
UNPUNCTUATED = frozenset({"th"})


def _target_done(text: str, lang: str) -> bool:
    return bool(text) and (lang in UNPUNCTUATED or not text[-1].isalnum())


def select_pair(english: dict[str, str], target: dict[str, str], lang: str) -> list[Translation]:
    """200 ID-aligned ``en_us`` -> ``lang`` pairs, in FLEURS id order."""
    aligned = {
        i
        for i in english.keys() & target.keys()
        if _is_clean_sentence(english[i]) and _target_done(target[i], lang)
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
