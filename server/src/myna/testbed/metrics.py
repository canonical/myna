"""Accuracy metrics: word- and character-error rate (T06).

Computed offline from a transcript and its reference — never during a run, so
the same record can be re-scored as normalization rules evolve. Latency
metrics live in ``harness.Metrics``; this module is purely about *what* was
said, not *when*.

Normalization (applied to both sides before scoring) is deliberately simple
and documented so results are reproducible:

  - Unicode NFKC, then casefold (lowercase).
  - Drop punctuation — anything that is not a word character or whitespace.
    Apostrophes inside words are kept so "don't" stays one token.
  - Collapse all whitespace runs to single spaces; strip ends.

This is the standard "clean" WER convention (no number expansion, no spelling
normalization). Anything fancier (spoken-number expansion, British/American
spelling folding) is a deliberate later decision, not baked in here.
"""

from __future__ import annotations

import re
import unicodedata
from dataclasses import dataclass

# Keep word chars, whitespace, and intra-word apostrophes; drop the rest.
_PUNCT = re.compile(r"[^\w\s']", flags=re.UNICODE)
_APOSTROPHE_EDGES = re.compile(r"(?<!\w)'|'(?!\w)")
_WS = re.compile(r"\s+")


def normalize(text: str) -> str:
    """Apply the documented normalization and return clean text."""
    text = unicodedata.normalize("NFKC", text).casefold()
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


def _align(reference: list, hypothesis: list) -> ErrorRate:
    """Levenshtein alignment with S/D/I backtrace over arbitrary token lists."""
    n, m = len(reference), len(hypothesis)
    if n == 0:
        return ErrorRate(0.0 if m == 0 else 1.0, 0, 0, m, 0)

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

    return ErrorRate((subs + dels + ins) / n, subs, dels, ins, n)


def word_error_rate(reference: str, hypothesis: str) -> ErrorRate:
    """WER between two strings after normalization."""
    return _align(normalize(reference).split(), normalize(hypothesis).split())


def character_error_rate(reference: str, hypothesis: str) -> ErrorRate:
    """CER between two strings after normalization (over characters)."""
    return _align(list(normalize(reference)), list(normalize(hypothesis)))
