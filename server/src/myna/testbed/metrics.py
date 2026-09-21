"""Accuracy metrics: word- and character-error rate (T06).

Computed offline from a transcript and its reference — never during a run, so
the same record can be re-scored as normalization rules evolve. Latency
metrics live in ``harness.Metrics``; this module is purely about *what* was
said, not *when*.

Normalization (applied to both sides before scoring) is deliberately simple
and documented so results are reproducible:

  - Unicode NFKC, then casefold (lowercase).
  - Fold typographic apostrophes (U+2019, U+2018, U+02BC) to ASCII "'" before
    punctuation handling, so a reference written with a curly quote (FLEURS
    French elisions use U+2019) scores against an ASCII hypothesis as the
    same word.
  - Drop punctuation — anything that is not a word character or whitespace.
    Apostrophes inside words are kept so "don't" stays one token.
  - Collapse all whitespace runs to single spaces; strip ends.

This is the standard "clean" WER convention (no number expansion, no spelling
normalization) - NVIDIA's own FLEURS cards use the same punctuation-and-case
convention, so this deliberately does not reach for Whisper's heavier
``EnglishTextNormalizer``/``BasicTextNormalizer``. Anything fancier
(spoken-number expansion, British/American spelling folding) is a deliberate
later decision, not baked in here.

``NORMALIZER_VERSION`` is stamped into every benchmark row (see
``myna.benchmarker._bench.to_line``) precisely because this function is
allowed to change: bump it whenever a change here can move a score, so rows
scored under different versions are never silently averaged together.
"""

from __future__ import annotations

import array
import re
import unicodedata
from collections.abc import Sequence
from dataclasses import dataclass

# Bump whenever `normalize` changes in a way that can move a WER/CER score.
NORMALIZER_VERSION = 1

# Keep word chars, whitespace, and intra-word apostrophes; drop the rest.
_PUNCT = re.compile(r"[^\w\s']", flags=re.UNICODE)
_APOSTROPHE_EDGES = re.compile(r"(?<!\w)'|'(?!\w)")
_WS = re.compile(r"\s+")

# RIGHT/LEFT SINGLE QUOTATION MARK and MODIFIER LETTER APOSTROPHE: the three
# characters a reference or hypothesis realistically uses for an elision or
# possessive apostrophe. Folded to ASCII "'" so they get the same intra-word
# handling below, regardless of which side (or transcript vendor) wrote which
# one. Left as-is by NFKC.
_TYPOGRAPHIC_APOSTROPHES = str.maketrans({"’": "'", "‘": "'", "ʼ": "'"})


def normalize(text: str) -> str:
    """Apply the documented normalization and return clean text."""
    text = unicodedata.normalize("NFKC", text).casefold()
    text = text.translate(_TYPOGRAPHIC_APOSTROPHES)
    text = _PUNCT.sub(" ", text)
    text = _APOSTROPHE_EDGES.sub(" ", text)  # leading/trailing quotes, not don't
    return _WS.sub(" ", text).strip()


@dataclass(frozen=True)
class ErrorRate:
    """An error rate with its edit breakdown.

    ``rate`` is ``(substitutions + deletions + insertions) / reference_length``
    and can exceed 1.0 when the hypothesis is much longer than the reference.
    ``reference_length`` of 0 yields a rate of 0.0 for an empty hypothesis and
    1.0 otherwise (nothing expected, anything is fully wrong).
    """

    rate: float
    substitutions: int
    deletions: int
    insertions: int
    reference_length: int

    @property
    def hits(self) -> int:
        return self.reference_length - self.substitutions - self.deletions


# Subproblems at or under this many cells are cheap enough (tens of KiB at
# worst) to solve with the direct O(n*m) table below; checkpointing them too
# would only add overhead for no benefit.
_DIRECT_CELL_BUDGET = 4096


def _dp_dtype(n: int, m: int) -> str:
    """The narrowest `array` typecode that can hold any edit distance up to
    max(n, m) - the trivial upper bound on the distance between sequences of
    these lengths (delete everything, insert everything) - without
    overflow. `array` elements are packed C values, not individually
    allocated Python ints, so this is most of where the memory goes back:
    2 or 4 bytes a cell instead of a full Python int object (~28 bytes) plus
    its list slot (~8 bytes)."""
    return "H" if max(n, m) < 65535 else "I"


def _row_after(
    prev_row: Sequence[int], ref_token: str, hypothesis: Sequence[str], typecode: str
) -> array.array[int]:
    """One forward DP step: dp[i][*] from dp[i-1][*], for a single reference
    token against the whole hypothesis. O(len(hypothesis)) time and memory."""
    cur = array.array(typecode)
    append = cur.append
    append(prev_row[0] + 1)
    for j in range(1, len(hypothesis) + 1):
        if ref_token == hypothesis[j - 1]:
            append(prev_row[j - 1])
        else:
            append(1 + min(prev_row[j - 1], prev_row[j], cur[j - 1]))
    return cur


def _direct_counts(reference: Sequence[str], hypothesis: Sequence[str]) -> tuple[int, int, int]:
    """Exact O(n*m)-memory DP and backtrace: the historical implementation,
    used as-is for small inputs (whole short clips), and as the primitive
    ``_checkpointed_counts`` below replays over one small block at a time."""
    n, m = len(reference), len(hypothesis)
    if n == 0:
        return (0, 0, m)
    if m == 0:
        return (0, n, 0)

    # dp[i][j] = edit distance between reference[:i] and hypothesis[:j]
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
                    dp[i - 1][j - 1],  # substitution
                    dp[i - 1][j],  # deletion
                    dp[i][j - 1],  # insertion
                )

    # Backtrace to count operation types.
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

    return subs, dels, ins


def _checkpointed_counts(
    reference: Sequence[str], hypothesis: Sequence[str]
) -> tuple[int, int, int]:
    """S/D/I counts in O(sqrt(n) * m) memory, byte-identical to
    ``_direct_counts`` (verified: server/tests/test_metrics.py runs both
    over an exhaustive small-case sweep and randomized larger ones - always
    equal, never merely "close").

    This is row checkpointing, not a Hirschberg *split*: a forward pass
    keeps only every `block`-th row of the DP table (compactly, via
    ``array``) instead of all of them. The backward pass then does exactly
    what ``_direct_counts``'s backtrace does - walk from (n, m) preferring a
    match, then a substitution, then a deletion, then an insertion - but one
    checkpoint-to-checkpoint block at a time: recompute that block's full
    local table from its checkpoint, backtrace through it, discard it, move
    to the block below. Because it recomputes the real table rather than
    guessing where an optimal path crosses a split (what a Hirschberg split
    would do, and why that was rejected: it can pick a different, differently
    tied S/D/I breakdown for the same edit distance), every tie is resolved
    from real neighboring cells, exactly as the single-table backtrace would.

    Every row of the table is computed exactly twice this way (once into a
    checkpoint, once inside its block's local backtrace), so this costs
    about 2x the O(n*m) table's time, for O(sqrt(n)) rows of memory instead
    of O(n) - block = sqrt(n) balances the checkpoint row count against the
    per-block replay width to minimize that memory.
    """
    n, m = len(reference), len(hypothesis)
    if n == 0:
        return (0, 0, m)
    if m == 0:
        return (0, n, 0)

    typecode = _dp_dtype(n, m)
    block = max(1, int(n**0.5))

    checkpoints: dict[int, array.array[int]] = {0: array.array(typecode, range(m + 1))}
    row = checkpoints[0]
    for i in range(1, n + 1):
        row = _row_after(row, reference[i - 1], hypothesis, typecode)
        if i % block == 0 or i == n:
            checkpoints[i] = row

    subs = dels = ins = 0
    i, j = n, m
    while i > 0 or j > 0:
        hi = i
        lo = ((hi - 1) // block) * block if hi > 0 else 0
        block_rows = [checkpoints[lo]]
        r = checkpoints[lo]
        for k in range(lo + 1, hi + 1):
            r = _row_after(r, reference[k - 1], hypothesis, typecode)
            block_rows.append(r)

        # Same backtrace as `_direct_counts`, indexed into this block's rows
        # (`local_i` = row index within the block) instead of a full table.
        local_i = i - lo
        while local_i > 0 or (lo == 0 and j > 0):
            cur_row = block_rows[local_i]
            # Only read when local_i > 0 (guarded below); block_rows[-1] here
            # is never reached in that case, so the fallback index is unused.
            prev_row = block_rows[local_i - 1 if local_i > 0 else 0]
            if local_i > 0 and j > 0 and reference[lo + local_i - 1] == hypothesis[j - 1]:
                local_i -= 1
                j -= 1
            elif local_i > 0 and j > 0 and cur_row[j] == prev_row[j - 1] + 1:
                subs += 1
                local_i -= 1
                j -= 1
            elif local_i > 0 and cur_row[j] == prev_row[j] + 1:
                dels += 1
                local_i -= 1
            else:
                ins += 1
                j -= 1
        i = lo

    return subs, dels, ins


def _align(reference: Sequence[str], hypothesis: Sequence[str]) -> ErrorRate:
    """Levenshtein alignment with S/D/I breakdown over arbitrary token lists.
    O(n*m) time either way (unchanged); memory is O(sqrt(n) * m) once the
    table is large enough for that to matter - see ``_checkpointed_counts``."""
    n, m = len(reference), len(hypothesis)
    if n == 0:
        return ErrorRate(0.0 if m == 0 else 1.0, 0, 0, m, 0)

    if n * len(hypothesis) <= _DIRECT_CELL_BUDGET:
        subs, dels, ins = _direct_counts(reference, hypothesis)
    else:
        subs, dels, ins = _checkpointed_counts(reference, hypothesis)
    return ErrorRate((subs + dels + ins) / n, subs, dels, ins, n)


def word_error_rate(reference: str, hypothesis: str) -> ErrorRate:
    """WER between two strings after normalization."""
    return _align(normalize(reference).split(), normalize(hypothesis).split())


def character_error_rate(reference: str, hypothesis: str) -> ErrorRate:
    """CER between two strings after normalization (over characters)."""
    return _align(list(normalize(reference)), list(normalize(hypothesis)))
