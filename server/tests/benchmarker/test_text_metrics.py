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


def test_edit_distance_known_values():
    for a, b, want in [
        ("flaw", "lawn", 2),
        ("sunday", "saturday", 3),
        ("abc", "", 3),
        ("abc", "abd", 1),
        ("ab", "ba", 2),
        ("", "", 0),
        ("a", "abcd", 3),
    ]:
        assert edit_distance(a, b) == edit_distance(b, a) == want


def test_deletion_budget_is_the_larger_of_two_and_half():
    assert conservative_edit_violation("a b c d", "a b") is None
    assert conservative_edit_violation("a b c d e", "a b") == "too-many-deletions"
    assert conservative_edit_violation("a b c d e f", "a b c") is None
    assert conservative_edit_violation("a b c d e f", "a b") == "too-many-deletions"


def test_a_failed_alignment_names_its_worst_deleted_run():
    long_run = "p q r s t u v"
    padding = " ".join(f"k{i}" for i in range(12))
    assert (
        conservative_edit_violation(f"not {padding} {long_run} tail", f"{padding} tail")
        == "span-too-long"
    )
    assert (
        conservative_edit_violation(f"{padding} not {padding} x y z", f"{padding} {padding}")
        == "isolated-negation"
    )
    assert (
        conservative_edit_violation(f"{padding} not {padding} not", f"{padding} {padding}")
        == "isolated-negation"
    )


def test_chrf_defaults_are_the_published_ones():
    hyps = ["Le chat est assis sur le tapis.", "Il fait beau aujourd'hui, non ?", "Bonsoir"]
    refs = ["Le chat s'est assis sur le tapis.", "Il fait beau aujourd'hui.", "Bonne soirée à tous"]
    assert chrf(hyps, refs) == pytest.approx(0.72746, abs=5e-6)
    assert chrf(hyps, refs, order=2) > chrf(hyps, refs)
    assert chrf(hyps, refs, beta=1) != chrf(hyps, refs)
    # recall is weighted: a longer hypothesis loses less than a shorter one
    assert chrf(["abcdef xyz"], ["abcdef"]) > chrf(["abcdef"], ["abcdef xyz"])


def test_percentile_edges():
    assert percentile([4.0, 1.0, 3.0, 2.0], 50) == 2.0
    assert percentile([4.0, 1.0, 3.0, 2.0], 75) == 3.0
    assert percentile([4.0, 1.0, 3.0, 2.0], 25) == 1.0
    assert percentile([4.0, 1.0, 3.0, 2.0], 0) == 1.0
    assert percentile([4.0, 1.0, 3.0, 2.0], 100) == 4.0


def test_a_six_word_run_does_not_hide_a_lone_negation():
    padding = " ".join(f"k{i}" for i in range(12))
    source = f"{padding} not {padding} p q r s t u {padding}"
    assert conservative_edit_violation(source, f"{padding} {padding} {padding}") == (
        "isolated-negation"
    )


def test_chrf_refuses_unpaired_inputs():
    with pytest.raises(ValueError, match="shorter"):
        chrf(["a"], [])
