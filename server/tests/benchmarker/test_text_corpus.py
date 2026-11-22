"""The vendored text corpus: its pins, its shape, and its reproducible build."""

from __future__ import annotations

import argparse
import re
import shutil

import pytest

from myna.benchmarker import text_corpus as tc
from myna.benchmarker.text_metrics import words


@pytest.fixture(scope="module")
def corpus():
    return tc.load()


def test_corpus_matches_its_sha256_pins():
    tc.verify()


def test_a_modified_slice_is_refused(tmp_path):
    for name in (*(f"{n}.jsonl" for n in tc.SLICES), tc.SUMS):
        shutil.copy(tc.CORPUS_DIR / name, tmp_path / name)
    path = tmp_path / "clean_en.jsonl"
    path.write_text(path.read_text(encoding="utf-8") + "\n", encoding="utf-8")
    with pytest.raises(tc.CorpusError, match="clean_en.jsonl"):
        tc.load(tmp_path)
    assert tc.load(tmp_path, check=False).clean_en


def test_clean_en_is_200_punctuated_cased_sentences(corpus):
    assert len(corpus.clean_en) == 200
    assert len({c.id for c in corpus.clean_en}) == 200
    for row in corpus.clean_en:
        assert row.text[0].isupper()
        assert row.text[-1] in ".?!"
        assert tc.MIN_WORDS <= len(row.text.split()) <= tc.MAX_WORDS


def test_translate_pairs_are_200_and_aligned_by_fleurs_id(corpus):
    rows = corpus.translate_en_fr
    assert len(rows) == 200
    assert len({r.id for r in rows}) == 200
    assert all(re.fullmatch(r"fleurs-\d+", r.id) for r in rows)
    assert all(r.en and r.fr and r.en != r.fr for r in rows)


def test_disfluent_en_spans_the_required_categories(corpus):
    pairs = corpus.disfluent_en
    assert len(pairs) >= 150
    assert len({p.id for p in pairs}) == len({p.heard for p in pairs}) == len(pairs)
    categories = {p.category for p in pairs}
    assert categories >= {
        "vocalized_pause",
        "repeat_k1",
        "repeat_k2",
        "repeat_k3",
        "repeat_k4",
        "reduplication_keep",
        "ambiguous_keep",
        "self_repair",
        "negation_trap",
        "negation_repair",
    }
    for category in categories:
        assert sum(p.category == category for p in pairs) >= 4


def test_keep_categories_are_identity_pairs(corpus):
    for pair in corpus.disfluent_en:
        if pair.category in ("reduplication_keep", "ambiguous_keep", "negation_trap"):
            assert pair.heard == pair.intended


def test_every_other_pair_is_an_edit(corpus):
    for pair in corpus.disfluent_en:
        if pair.category not in ("reduplication_keep", "ambiguous_keep", "negation_trap"):
            assert pair.heard != pair.intended, pair.id


def _has_tandem_repeat(tokens: list[str], k: int) -> bool:
    return any(
        tokens[i : i + k] == tokens[i + k : i + 2 * k] for i in range(len(tokens) - 2 * k + 1)
    )


def test_repeat_pairs_hold_a_tandem_repeat_of_their_length(corpus):
    for pair in corpus.disfluent_en:
        match = re.fullmatch(r"repeat_k(\d)", pair.category)
        if match:
            assert _has_tandem_repeat(words(pair.heard), int(match[1])), pair.id
            assert len(words(pair.heard)) == len(words(pair.intended)) + int(match[1])


def test_corpus_stays_small():
    total = sum((tc.CORPUS_DIR / f"{n}.jsonl").stat().st_size for n in tc.SLICES)
    assert total < 160_000


def _tsv(rows):
    return "".join(f"{i}\t{i}.wav\t{raw}\tnorm\tchars\t1\tMALE\n" for i, raw in rows)


def _synthetic(tmp_path, count=260):
    english = [(i, f"This is the English sentence number {i} for testing.") for i in range(count)]
    french = [(i, f"Ceci est la phrase française numéro {i} pour le test.") for i in range(count)]
    for locale, rows in (("en_us", english), ("fr_fr", french[: count - 20])):
        (tmp_path / locale).mkdir()
        (tmp_path / locale / "test.tsv").write_text(_tsv(rows), encoding="utf-8")
    return english


def test_fleurs_tsv_keeps_one_sentence_per_id_and_skips_blank_rows(tmp_path):
    path = tmp_path / "test.tsv"
    path.write_text(
        _tsv([("7", "Seven."), ("7", "Seven."), ("8", "  ")]) + "short\n", encoding="utf-8"
    )
    assert tc.read_fleurs_tsv(path) == {"7": "Seven."}


def test_fleurs_tsv_refuses_conflicting_transcripts(tmp_path):
    path = tmp_path / "test.tsv"
    path.write_text(_tsv([("7", "Seven."), ("7", "Seventeen.")]), encoding="utf-8")
    with pytest.raises(tc.CorpusError, match="sentence 7"):
        tc.read_fleurs_tsv(path)


def test_build_is_deterministic_and_selects_aligned_ids(tmp_path):
    _synthetic(tmp_path)
    english = tc.read_fleurs_tsv(tmp_path / "en_us" / "test.tsv")
    french = tc.read_fleurs_tsv(tmp_path / "fr_fr" / "test.tsv")
    clean, translate = tc.select_fleurs(english, french)
    assert (clean, translate) == tc.select_fleurs(english, french)
    assert len(clean) == tc.N_CLEAN and len(translate) == tc.N_TRANSLATE
    assert [int(t.id.split("-")[1]) for t in translate] == sorted(
        int(t.id.split("-")[1]) for t in translate
    )
    assert all(int(t.id.split("-")[1]) in range(240) for t in translate)


def test_too_few_sentences_is_an_error(tmp_path):
    with pytest.raises(tc.CorpusError, match="too few"):
        tc.select_fleurs({"1": "Short sentence is here, ok."}, {"1": "Phrase."})


def test_build_command_writes_slices_and_pins(tmp_path):
    fleurs, out = tmp_path / "fleurs", tmp_path / "out"
    fleurs.mkdir()
    out.mkdir()
    _synthetic(fleurs)
    shutil.copy(tc.CORPUS_DIR / "disfluent_en.jsonl", out)
    tc.cmd_build(argparse.Namespace(fleurs=fleurs, out=out))
    built = tc.load(out)
    assert len(built.clean_en) == 200 and len(built.translate_en_fr) == 200
    first = (out / "clean_en.jsonl").read_bytes()
    tc.cmd_build(argparse.Namespace(fleurs=fleurs, out=out))
    assert (out / "clean_en.jsonl").read_bytes() == first
