"""WER/CER accuracy metrics (T06). Pure functions, no model or audio."""

from myna.testbed.metrics import (
    character_error_rate,
    normalize,
    word_error_rate,
)


def test_normalize_strips_punctuation_and_case_but_keeps_contractions():
    assert normalize("  Don't STOP, please! ") == "don't stop please"


def test_normalize_folds_unicode_width_and_quotes():
    # fullwidth letters -> ascii (NFKC), smart quotes treated as edge punctuation
    assert normalize("‘Ｈello’") == "hello"


def test_wer_is_zero_for_identical_after_normalization():
    er = word_error_rate("Turn the volume up.", "turn the volume up")
    assert er.rate == 0.0
    assert (er.substitutions, er.deletions, er.insertions) == (0, 0, 0)


def test_wer_counts_one_substitution():
    er = word_error_rate("the quick brown fox", "the quick green fox")
    assert er.substitutions == 1
    assert er.deletions == 0 and er.insertions == 0
    assert er.rate == 1 / 4
    assert er.hits == 3


def test_wer_counts_deletion_and_insertion():
    deleted = word_error_rate("a b c d", "a c d")  # dropped "b"
    assert (deleted.substitutions, deleted.deletions, deleted.insertions) == (0, 1, 0)

    inserted = word_error_rate("a c d", "a b c d")  # added "b"
    assert (inserted.substitutions, inserted.deletions, inserted.insertions) == (0, 0, 1)


def test_empty_reference_edges():
    assert word_error_rate("", "").rate == 0.0
    assert word_error_rate("", "spurious words").rate == 1.0  # nothing expected


def test_cer_subword():
    er = character_error_rate("kitten", "sitting")  # classic: 3 edits / 6 chars
    assert er.substitutions + er.deletions + er.insertions == 3
    assert er.reference_length == 6
    assert er.rate == 3 / 6
