"""WER/CER accuracy metrics (T06). Pure functions, no model or audio."""

from __future__ import annotations

import random
import tracemalloc

import pytest
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

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


# --- Memory: _align used to be a full O(n*m) table (see finding a5-rss:
# ~380 MB scoring a 5-minute clip's CER). It is now row-checkpointed
# (metrics._checkpointed_counts): O(sqrt(n) * m) memory instead of O(n * m),
# by recomputing most of the table from saved checkpoint rows instead of
# keeping it all. Unlike a Hirschberg *split* (which was tried and rejected -
# see the commit body), checkpointing recomputes the real table rather than
# guessing where an optimal path crosses a midpoint, so it is byte-identical
# to the old implementation, not just equal in total edit distance.
# `_oracle_align` below is a verbatim copy of the pre-optimization
# implementation, kept independent of metrics.py, so these tests catch a
# regression in either the new code or an accidental edit to the oracle.


def _oracle_align(reference: list[str], hypothesis: list[str]) -> ErrorRate:
    """The original O(n*m)-memory DP and single-pass backtrace, copied
    verbatim (not called from metrics.py) so it stays an independent oracle
    for the memory-bounded Hirschberg implementation."""
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


_FORCE_CHECKPOINT_HEALTHCHECK = settings(
    max_examples=40,
    deadline=None,
    suppress_health_check=[HealthCheck.function_scoped_fixture],
)
# Safe: `monkeypatch.setattr` is called fresh at the start of every generated
# example, not relied on to reset between them, so the fixture's normal
# per-test (not per-example) teardown timing doesn't matter here.


@given(
    seed=st.integers(min_value=0, max_value=2**32 - 1),
    ref_len=st.integers(min_value=0, max_value=150),
    edits=st.integers(min_value=0, max_value=20),
    budget=st.sampled_from([1, 50, 4096]),
)
@_FORCE_CHECKPOINT_HEALTHCHECK
def test_checkpointed_matches_oracle_on_mutated_word_sequences(
    monkeypatch, seed, ref_len, edits, budget
):
    """A realistic reference/hypothesis pair (a mutated copy of the
    reference, as a real transcript is): the new alignment must return the
    exact same rate/S/D/I as the pre-optimization table, whether or not
    checkpointing is forced on for small inputs."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", budget)
    rng = random.Random(seed)
    vocabulary = [f"w{i}" for i in range(12)]
    reference = [rng.choice(vocabulary) for _ in range(ref_len)]
    hypothesis = _mutate(reference, vocabulary, edits, rng)

    got = metrics_mod._align(reference, hypothesis)
    want = _oracle_align(reference, hypothesis)
    assert got == want


@given(
    seed=st.integers(min_value=0, max_value=2**32 - 1),
    ref_len=st.integers(min_value=0, max_value=150),
    edits=st.integers(min_value=0, max_value=20),
    budget=st.sampled_from([1, 50, 4096]),
)
@_FORCE_CHECKPOINT_HEALTHCHECK
def test_checkpointed_matches_oracle_on_mutated_character_sequences(
    monkeypatch, seed, ref_len, edits, budget
):
    """Same as above at character granularity (CER): a small alphabet makes
    coincidental matches, and therefore alignment ties, much more common
    than at word granularity."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", budget)
    rng = random.Random(seed)
    alphabet = list("abcde ")
    reference = [rng.choice(alphabet) for _ in range(ref_len)]
    hypothesis = _mutate(reference, alphabet, edits, rng)

    got = metrics_mod._align(reference, hypothesis)
    want = _oracle_align(reference, hypothesis)
    assert got == want


@given(
    seed=st.integers(min_value=0, max_value=2**32 - 1),
    ref_len=st.integers(min_value=0, max_value=12),
    hyp_len=st.integers(min_value=0, max_value=12),
)
@settings(
    max_examples=150,
    deadline=None,
    suppress_health_check=[HealthCheck.function_scoped_fixture],
)
def test_checkpointed_matches_oracle_on_fully_independent_short_sequences(
    monkeypatch, seed, ref_len, hyp_len
):
    """No shared derivation between the two sides at all (over a 3-letter
    alphabet, so ties - including transposition-style ties such as "abc" vs
    "acb", where 2 substitutions cost the same as 1 deletion + 1 insertion -
    are frequent) and the budget forced to 1 so every pair is checkpointed.
    This is the harshest tie-break stress test the two implementations are
    put through, and exactly the case a Hirschberg-split approach failed on
    during development (see the commit body) - checkpointing does not."""
    monkeypatch.setattr(metrics_mod, "_DIRECT_CELL_BUDGET", 1)
    rng = random.Random(seed)
    alphabet = list("abc")
    reference = [rng.choice(alphabet) for _ in range(ref_len)]
    hypothesis = [rng.choice(alphabet) for _ in range(hyp_len)]

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


def test_wer_memory_stays_bounded_on_a_long_form_clip():
    """A ~900-word long-form reference (finding a5-rss's ballpark) must score
    in a small, roughly-constant amount of memory, not the O(n*m) table the
    old implementation used."""
    reference, hypothesis = _long_form_pair(word_count=900, error_rate=0.08, seed=1)
    tracemalloc.start()
    try:
        word_error_rate(reference, hypothesis)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 4 << 20  # 4 MiB; the old table alone would be tens of MiB here


def test_cer_memory_stays_bounded_on_a_long_form_clip():
    """Character-level scoring is where the O(n*m) table actually hurt (a5-rss:
    a 5-minute clip's ~3539 x ~3640 CER matrix cost ~380 MB): thousands of
    characters per side, over a small alphabet where coincidental matches
    (and therefore ties) are common."""
    reference, hypothesis = _long_form_pair(word_count=900, error_rate=0.08, seed=2)
    tracemalloc.start()
    try:
        character_error_rate(reference, hypothesis)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 4 << 20  # 4 MiB; the retired table was ~380 MB for a clip this size


def test_cer_memory_stays_bounded_at_roughly_twice_long_form_scale():
    """Memory must not grow with the square of the transcript length: at
    ~2x the characters of the long-form clip above, peak memory should stay
    in the same small ballpark, not ~4x it. (A much larger corpus clip is
    measured separately, outside the suite - see the commit body - where an
    O(n*m)-time pure-Python DP, old or new, is too slow to run on every
    `make test-server`.)"""
    reference, hypothesis = _long_form_pair(word_count=1800, error_rate=0.08, seed=3)
    tracemalloc.start()
    try:
        character_error_rate(reference, hypothesis)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 8 << 20  # 8 MiB - roughly sqrt(2)x the ~900-word case above, not 4x
