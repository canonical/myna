"""Metrics of the text evaluation suite: edit distance, polarity, the
ConservativeEdit reference check, chrF and percentiles."""

from __future__ import annotations

import json
from pathlib import Path

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
        ("", "", "empty"),
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
    # A vocabulary word replaces heard words that start as it does; it is not
    # an insertion anywhere.
    assert conservative_edit_violation("gee nome here", "Gnome here", ["Gnome"]) is None
    assert conservative_edit_violation("gee nome here", "Gnome here") == "not-a-subsequence"
    assert conservative_edit_violation("ship it", "ship it Gnome", ["Gnome"]) == (
        "not-a-subsequence"
    )
    assert conservative_edit_violation("restart my sequel", "MySQL", ["my sequel => MySQL"]) is None


def test_a_written_word_may_not_replace_a_lone_negation():
    assert conservative_edit_violation("we do not ship", "we do nginx ship", ["nginx"]) == (
        "isolated-negation"
    )
    assert conservative_edit_violation("run not now", "run nginx now", ["nginx"]) == (
        "isolated-negation"
    )


def test_the_words_a_written_word_replaces_count_as_deleted():
    # Four of five heard words become one: three deleted, past the budget of
    # two once a fifth word goes too.
    assert conservative_edit_violation("a b my sequel server", "MySQL", ["MySQL"]) == (
        "too-many-deletions"
    )


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
    # Half rounds up, as in the product: five words may lose three.
    assert conservative_edit_violation("a b c d e", "a b") is None
    assert conservative_edit_violation("a b c d e f g", "a b") == "too-many-deletions"
    assert conservative_edit_violation("a b c d e f", "a b c") is None
    assert conservative_edit_violation("a b c d e f", "a b") == "too-many-deletions"


def test_a_failed_alignment_names_the_first_reason_it_met():
    long_run = "p q r s t u v"
    padding = " ".join(f"k{i}" for i in range(12))
    assert (
        conservative_edit_violation(f"{padding} {long_run} tail", f"{padding} tail")
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


# The Rust ``Check::ConservativeEdit`` (client/myna-desktop/src/process/check.rs)
# is the product's check; this file's cases are run through it by a Rust test
# that regenerates them from the vendored corpus (``MYNA_REGEN_CASES=1``).
_CASES = (
    Path(__file__).resolve().parents[3]
    / "client/myna-desktop/tests/data/conservative_edit_cases.json"
)
_VERDICTS = {
    "not-a-subsequence": "new-word",
    "too-many-deletions": "too-many-dropped",
    "span-too-long": "long-drop",
    "isolated-negation": "dropped-negation",
    "empty": "empty",
    None: "ok",
}


@pytest.mark.repo_tree
def test_the_reference_check_agrees_with_the_products_on_every_shared_case():
    cases = json.loads(_CASES.read_text(encoding="utf-8"))
    assert len(cases) > 500
    wrong = [
        (c["heard"], c["answer"], c["written"], c["verdict"], got)
        for c in cases
        if (got := _VERDICTS[conservative_edit_violation(c["heard"], c["answer"], c["written"])])
        != c["verdict"]
    ]
    assert not wrong, f"{len(wrong)} of {len(cases)} differ, first: {wrong[:3]}"


from myna.benchmarker.text_metrics import (  # noqa: E402
    copy_through_rate,
    empty_rate,
    has_repeat_loop,
    median_length_ratio,
    repeat_loop_rate,
)


def test_empty_rate_counts_outputs_with_no_letter_or_digit():
    assert empty_rate(["Bonjour.", "", "  ", "...", "42"]) == pytest.approx(3 / 5)
    assert empty_rate([]) == 0.0
    assert empty_rate(["日本語"]) == 0.0


@pytest.mark.parametrize(
    ("text", "loops"),
    [
        ("the the the the", True),
        ("the the the", False),
        ("I said yes. I said yes. I said yes. I said yes.", True),
        ("a b c d e f a b c d e f a b c d e f a b c d e f", True),
        ("a b c d e f g a b c d e f g a b c d e f g a b c d e f g", False),
        ("Nobody repeats themselves here at all.", False),
        ("the the the then the", False),
        ("a b a c a b a b", False),
        ("aaaaaaa", True),
        ("aaaaaaa b", False),
        ("ははははははは", True),
        ("ああああ", False),
        ("abab", False),
        ("", False),
    ],
)
def test_repeat_loops_need_four_back_to_back_copies(text, loops):
    assert has_repeat_loop(text) is loops


def test_repeat_loop_rate_is_the_share_of_looping_outputs():
    assert repeat_loop_rate(["ok", "x x x x", "fine", "fine"]) == 0.25
    assert repeat_loop_rate([]) == 0.0


def test_median_length_ratio_is_against_the_reference_and_ignores_empty_references():
    assert median_length_ratio(["aa", "bbbb", "c"], ["aa", "bb", "cc"]) == pytest.approx(1.0)
    assert median_length_ratio(["a", "bbbb"], ["aa", "bb"]) == pytest.approx(1.25)
    assert median_length_ratio(["xx"], ["  "]) == 0.0
    assert median_length_ratio(["xx", "  yyyy  "], ["xx", "yy"]) == pytest.approx(1.5)
    five = median_length_ratio(["a", "aa", "aaa", "aaaa", "aaaaa"], ["a"] * 5)
    assert five == 3.0
    four = median_length_ratio(["a", "aa", "aaa", "aaaa"], ["a"] * 4)
    assert four == 2.5
    with pytest.raises(ValueError):
        median_length_ratio(["a"], [])
    with pytest.raises(ValueError):
        median_length_ratio([], ["a"])


def test_copy_through_ignores_case_and_punctuation():
    outs = ["Hello, world!", "Bonjour le monde.", "hello world"]
    srcs = ["hello world", "Hello world.", "Hello World."]
    assert copy_through_rate(outs, srcs) == pytest.approx(2 / 3)
    assert copy_through_rate([], []) == 0.0
    with pytest.raises(ValueError):
        copy_through_rate(["a"], [])
