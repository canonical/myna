"""WER/CER accuracy metrics (T06). Pure functions, no model or audio."""

from __future__ import annotations

import random
import tracemalloc

import pytest

from myna.testbed import metrics as metrics_mod
from myna.testbed.metrics import (
    ErrorRate,
    character_error_rate,
    normalize,
    word_error_rate,
)


def test_normalize_strips_punctuation_and_case_but_keeps_contractions():
    assert normalize("  Don't STOP, please! ") == "don't stop please"


def test_normalize_folds_unicode_width_and_quotes():
    # fullwidth letters -> ascii (NFKC), smart quotes treated as edge punctuation
    assert normalize("‘Ｈello’") == "hello"


def test_normalize_folds_typographic_apostrophes_inside_words():
    # FLEURS fr references use U+2019 for elisions; ASCII hypotheses must not
    # split "l'accident" into two words against them.
    assert normalize("L’accident") == normalize("L'accident") == "l'accident"
    assert normalize("L‘accident") == "l'accident"  # common curly-quote typo
    assert normalize("Lʼaccident") == "l'accident"  # modifier letter apostrophe


def test_wer_zero_between_typographic_and_ascii_apostrophe():
    er = word_error_rate("L’accident", "L'accident")
    assert er.rate == 0.0
    assert (er.substitutions, er.deletions, er.insertions) == (0, 0, 0)


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


# --- Correctness and memory: _align used to be a full O(n*m) table (see
# finding a5-rss: ~380 MB scoring a 5-minute clip's CER). It is now
# row-checkpointed (metrics._checkpointed_counts): O(sqrt(n) * m) memory
# instead of O(n * m), by recomputing most of the table from saved
# checkpoint rows instead of keeping it all. Unlike a Hirschberg *split*
# (tried and rejected - see the commit body), checkpointing recomputes the
# real table rather than guessing where an optimal path crosses a midpoint,
# so it is byte-identical to the old implementation, not just equal in
# total edit distance.
#
# `_oracle_align` below is a verbatim copy of the pre-optimization
# implementation, kept independent of metrics.py, so these tests catch a
# regression in either the new code or an accidental edit to the oracle.
# What follows is deliberately a small, fast, deterministic set of cases,
# not a property test: an exhaustive sweep (3.6M small cases) and large
# randomized runs (the ones that actually established byte-identical
# behavior against the oracle, and, separately, that a Hirschberg split
# does not have it) live in
# /home/charles/myna-audio-review/evidence/a5-metrics/ instead - they took
# minutes to run and only needed to convince us once. A suite every
# developer and CI run pays for stays a few seconds.


def _oracle_align(reference: list[str], hypothesis: list[str]) -> ErrorRate:
    """The original O(n*m)-memory DP and single-pass backtrace, copied
    verbatim (not called from metrics.py) so it stays an independent oracle
    for the memory-bounded checkpointed implementation."""
    n, m = len(reference), len(hypothesis)
    if n == 0:
        return ErrorRate(0.0 if m == 0 else 1.0, 0, 0, m, 0)

    dp = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        dp[i][0] = i
    for j in range(m + 1):
        dp[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            if reference[i - 1] == hypothesis[j - 1]:
                dp[i][j] = dp[i - 1][j - 1]
            else:
                dp[i][j] = 1 + min(
                    dp[i - 1][j - 1],
                    dp[i - 1][j],
                    dp[i][j - 1],
                )

    i, j = n, m
    subs = dels = ins = 0
    while i > 0 or j > 0:
        if i > 0 and j > 0 and reference[i - 1] == hypothesis[j - 1]:
            i, j = i - 1, j - 1
        elif i > 0 and j > 0 and dp[i][j] == dp[i - 1][j - 1] + 1:
            subs += 1
            i, j = i - 1, j - 1
        elif i > 0 and dp[i][j] == dp[i - 1][j] + 1:
            dels += 1
            i -= 1
        else:
            ins += 1
            j -= 1

    return ErrorRate((subs + dels + ins) / n, subs, dels, ins, n)


def _mutate(tokens: list[str], vocabulary: list[str], edits: int, rng: random.Random) -> list[str]:
    """Apply `edits` random substitute/delete/insert operations to `tokens`,
    the way a real ASR hypothesis differs from its reference: mostly-correct
    with a handful of local errors, not an unrelated random sequence."""
    out = list(tokens)
    for _ in range(edits):
        if not out or rng.random() < 0.34:
            pos = rng.randrange(len(out) + 1)
            out.insert(pos, rng.choice(vocabulary))
        elif rng.random() < 0.5:
            del out[rng.randrange(len(out))]
        else:
            out[rng.randrange(len(out))] = rng.choice(vocabulary)
    return out


@pytest.mark.parametrize(
    "budget,seed,ref_len,edits,alphabet",
    [
        pytest.param(1, 1, 40, 8, "word", id="word-tiny-budget"),
        pytest.param(1, 2, 40, 8, "char", id="char-tiny-budget"),
        pytest.param(11, 3, 90, 12, "word", id="word-small-block"),
        pytest.param(11, 4, 90, 12, "char", id="char-small-block"),
        pytest.param(4096, 5, 90, 12, "word", id="word-direct-table"),
        pytest.param(4096, 6, 90, 12, "char", id="char-direct-table"),
    ],
)
def test_checkpointed_matches_oracle_on_mutated_sequences(
    monkeypatch, budget, seed, ref_len, edits, alphabet
):
    """A realistic reference/hypothesis pair (a mutated copy of the
    reference, as a real transcript is), at a handful of hand-picked
    (budget, size) shapes: checkpointing forced on for a tiny input, a
    small checkpoint block on a mid-size input, and the default budget's
    direct-table path - at both word and character granularity (a small
    alphabet makes coincidental matches, and therefore ties, more common
    at character granularity). Deterministic and fast; the exhaustive
    version lives in the evidence directory (see the module docstring)."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", budget)
    rng = random.Random(seed)
    vocabulary = [f"w{i}" for i in range(12)] if alphabet == "word" else list("abcde ")
    reference = [rng.choice(vocabulary) for _ in range(ref_len)]
    hypothesis = _mutate(reference, vocabulary, edits, rng)

    got = metrics_mod._align(reference, hypothesis)
    want = _oracle_align(reference, hypothesis)
    assert got == want


@pytest.mark.parametrize("seed", range(20))
def test_checkpointed_matches_oracle_on_fully_independent_short_sequences(monkeypatch, seed):
    """No shared derivation between the two sides at all (over a 3-letter
    alphabet, so ties - including transposition-style ties such as "abc" vs
    "acb", where 2 substitutions cost the same as 1 deletion + 1 insertion -
    are frequent), budget forced to 1 so every pair is checkpointed. This is
    the harshest tie-break stress test the two implementations are put
    through, and exactly the case a Hirschberg-split approach failed on
    during development (see the commit body) - checkpointing does not.
    20 fixed seeds, not a property test: fast and reproducible, with the
    full randomized sweep in the evidence directory."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", 1)
    rng = random.Random(seed)
    alphabet = list("abc")
    reference = [rng.choice(alphabet) for _ in range(rng.randint(0, 12))]
    hypothesis = [rng.choice(alphabet) for _ in range(rng.randint(0, 12))]

    got = metrics_mod._align(reference, hypothesis)
    want = _oracle_align(reference, hypothesis)
    assert got == want


def test_checkpointed_matches_oracle_on_a_known_transposition_tie(monkeypatch):
    """ "abc" -> "acb": 2 substitutions and 1 deletion + 1 insertion are both
    valid cost-2 decompositions - the single-table backtrace picks the
    2-substitution one. Forced through the checkpointed path (budget=1) to
    exercise the tie at the point it is riskiest."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", 1)
    reference, hypothesis = list("abc"), list("acb")
    got = metrics_mod._align(reference, hypothesis)
    want = _oracle_align(reference, hypothesis)
    assert got == want == ErrorRate(2 / 3, 2, 0, 0, 3)


@pytest.mark.parametrize("cell_budget", [1, 4096])
def test_checkpointed_matches_original_on_existing_cases_regardless_of_budget(
    monkeypatch, cell_budget
):
    """The examples above already cover these at the default budget; this
    pins them at the extremes (always-checkpointed and never-checkpointed)
    too."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", cell_budget)
    assert word_error_rate("the quick brown fox", "the quick green fox") == ErrorRate(
        1 / 4, 1, 0, 0, 4
    )
    assert word_error_rate("a b c d", "a c d") == ErrorRate(1 / 4, 0, 1, 0, 4)
    assert word_error_rate("a c d", "a b c d") == ErrorRate(1 / 3, 0, 0, 1, 3)
    assert character_error_rate("kitten", "sitting") == ErrorRate(1 / 2, 2, 0, 1, 6)


# Short, common-length words (not "word123"-style tokens), so word_count=900
# lands on the same ~3.5-4k character scale a5-rss actually measured
# (3539 x ~3640) rather than an inflated one.
_REALISTIC_VOCABULARY = (
    "the a is of and to in that it for was on with he as you do at this but "
    "his by from they we say her she or an will my one all would there their "
    "what so up out if about who get which go me when make can like time no "
    "just him know take people into year your good some could them see other"
).split()


def _long_form_pair(word_count: int, error_rate: float, seed: int) -> tuple[str, str]:
    rng = random.Random(seed)
    reference = [rng.choice(_REALISTIC_VOCABULARY) for _ in range(word_count)]
    hypothesis = _mutate(reference, _REALISTIC_VOCABULARY, int(word_count * error_rate), rng)
    return " ".join(reference), " ".join(hypothesis)


def test_cer_memory_stays_bounded_on_a_long_form_clip():
    """The one memory test that matters: character-level scoring is where
    the O(n*m) table actually hurt (a5-rss: a 5-minute clip's ~3539 x
    ~3640 CER matrix cost ~380 MB) - thousands of characters per side, over
    a small alphabet where coincidental matches (and therefore ties) are
    common. One representative long-form-scale pair; a larger corpus clip
    and the word-level case are measured separately, outside the suite (see
    the evidence directory and the commit body) - an O(n*m)-time
    pure-Python DP, old or new, is too slow at that scale to run on every
    `make test-server`."""
    reference, hypothesis = _long_form_pair(word_count=900, error_rate=0.08, seed=2)
    tracemalloc.start()
    try:
        character_error_rate(reference, hypothesis)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 4 << 20  # 4 MiB; the retired table was ~380 MB for a clip this size
