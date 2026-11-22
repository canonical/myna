"""Metrics of the text evaluation suite: edit distance, polarity, the
ConservativeEdit reference check, chrF and percentiles."""

from __future__ import annotations

import pytest

from myna.benchmarker.text_metrics import (
    chrf,
    conservative_edit_violation,
    edit_distance,
    negation_count,
    percentile,
    polarity_flip,
)


def test_edit_distance_counts_case_and_punctuation():
    assert edit_distance("Hello, world.", "Hello, world.") == 0
    assert edit_distance("Hello, world.", "hello world") == 3
    assert edit_distance("", "abc") == 3
    assert edit_distance("kitten", "sitting") == 3
    assert edit_distance("ab", "abcd") == edit_distance("abcd", "ab") == 2


def test_negation_count_sees_contractions():
    assert negation_count("I don't think we should not, never, cannot") == 4
    assert negation_count("Another note") == 0


def test_polarity_flip_detects_dropped_and_added_negations():
    heard, intended = "Do not merge this.", "Do not merge this."
    assert not polarity_flip(heard, intended, "Do not merge this.")
    assert polarity_flip(heard, intended, "Do merge this.")
    assert polarity_flip("Merge this.", "Merge this.", "Do not merge this.")


def test_polarity_flip_allows_a_repair_to_drop_its_own_negation():
    heard = "We shouldn't, wait, we should ship."
    assert not polarity_flip(heard, "We should ship.", "We should ship.")


@pytest.mark.parametrize(
    ("source", "output", "reason"),
    [
        ("Um I think so", "I think so", None),
        ("Monday, no, Tuesday", "Tuesday", None),
        ("we shouldn't, wait, we should ship on friday", "we should ship on friday", None),
        ("do not merge this", "do merge this", "isolated-negation"),
        ("it isn't ready", "it ready", "isolated-negation"),
        ("one two three four five six", "one", "too-many-deletions"),
        ("alpha beta", "gamma", "not-a-subsequence"),
        ("alpha beta", "beta alpha", "not-a-subsequence"),
        ("alpha beta", "alpha beta gamma", "not-a-subsequence"),
        ("alpha beta", "", "empty"),
        ("", "", None),
    ],
)
def test_conservative_edit_reference_check(source, output, reason):
    assert conservative_edit_violation(source, output) == reason


def test_conservative_edit_accepts_any_legal_alignment():
    # Keeping either "on" is a legal reading; only one leaves "not" alone.
    assert (
        conservative_edit_violation(
            "let's meet not on monday no on tuesday afternoon", "let's meet on tuesday afternoon"
        )
        is None
    )


def test_conservative_edit_caps_a_deleted_run_at_six_words():
    source = "a b c d e f g h i j k l m n"
    assert conservative_edit_violation(source, "a b c d e f g h") is None
    assert conservative_edit_violation(source + " o", "a b c d e f g h") == "span-too-long"


def test_conservative_edit_allows_written_words_to_expand():
    assert conservative_edit_violation("ship it", "ship it Gnome", ["gnome"]) is None
    assert conservative_edit_violation("ship it", "ship it Gnome") == "not-a-subsequence"


def test_chrf_is_one_for_identical_and_low_for_unrelated():
    assert chrf(["Bonjour le monde."], ["Bonjour le monde."]) == pytest.approx(1.0)
    assert chrf(["xxxxxxxx"], ["Bonjour le monde."]) < 0.05
    assert chrf([""], ["Bonjour"]) == 0.0


def test_chrf_ignores_whitespace_but_not_case():
    assert chrf(["a b c d e f g"], ["abcdefg"]) == pytest.approx(1.0)
    assert chrf(["ABCDEFG"], ["abcdefg"]) < 0.01


def test_chrf_matches_sacrebleu():
    sacrebleu = pytest.importorskip("sacrebleu")
    hyps = ["Le chat est assis sur le tapis.", "Il fait beau aujourd'hui, non ?", "Bonsoir"]
    refs = ["Le chat s'est assis sur le tapis.", "Il fait beau aujourd'hui.", "Bonne soirée à tous"]
    expected = sacrebleu.corpus_chrf(hyps, [refs]).score / 100
    assert chrf(hyps, refs) == pytest.approx(expected, abs=1e-9)


def test_percentile_is_nearest_rank():
    values = [float(v) for v in range(1, 101)]
    assert percentile(values, 50) == 50.0
    assert percentile(values, 95) == 95.0
    assert percentile(values, 100) == 100.0
    assert percentile([7.0], 95) == 7.0
    assert percentile([], 95) == 0.0
