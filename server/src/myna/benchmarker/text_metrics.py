"""Text-only metrics for the text evaluation suite (``myna-bench text``).

Stdlib only, so they ride in the zipapp. Unlike the ASR WER normaliser
(``myna.testbed.metrics``), the edit and chrF scores here are case- and
punctuation-aware: a cleanup step that lowercases a sentence or drops its
full stop has damaged it, and a WER metric would never notice.
"""

from __future__ import annotations

from collections import Counter
from collections.abc import Sequence
from functools import cache

from myna.testbed.metrics import normalize


# Word tokens after the ASR normaliser: lowercased, punctuation dropped,
# intra-word apostrophes kept, so "shouldn't" is one token.
def words(text: str) -> list[str]:
    return normalize(text).split()


def is_negation(token: str) -> bool:
    return token in ("not", "never", "cannot") or token.endswith("n't")


def negation_count(text: str) -> int:
    return sum(1 for token in words(text) if is_negation(token))


def edit_distance(a: str, b: str) -> int:
    """Levenshtein distance over characters (case and punctuation count)."""
    if len(a) < len(b):
        a, b = b, a
    previous = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        current = [i]
        for j, cb in enumerate(b, 1):
            current.append(min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + (ca != cb)))
        previous = current
    return previous[-1]


def polarity_flip(heard: str, intended: str, output: str) -> bool:
    """Whether ``output`` reversed the meaning: it dropped a negation the
    intended text keeps, or it carries one the speaker never said."""
    got = negation_count(output)
    return got < negation_count(intended) or got > negation_count(heard)


_UNSPACED = (
    (0x3040, 0x30FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xF900, 0xFAFF),
    (0x20000, 0x2FA1F),
)
_LONGEST_DROP = 6
_LONGEST_MISHEARING = 3
_SPOKEN_WRITTEN_SEPARATOR = " => "


def _is_unspaced(c: str) -> bool:
    """Ideographs and kana: written without spaces, so each is a word."""
    return any(lo <= ord(c) <= hi for lo, hi in _UNSPACED)


def _check_words(text: str) -> list[str]:
    """The words ``Check::ConservativeEdit`` compares: lowercased, stripped of
    everything but letters, digits and apostrophes (a curly one is straight),
    with apostrophes trimmed from the edges, and each ideograph or kana a word
    of its own. Not the ASR normaliser, which treats punctuation inside a token
    differently."""
    tokens: list[str] = []
    word = ""

    def flush() -> None:
        nonlocal word
        kept = word.strip("'")
        if kept:
            tokens.append(kept)
        word = ""

    for raw in text.split():
        for c in raw.replace("\u2019", "'"):
            if _is_unspaced(c):
                flush()
                word = c
                flush()
            elif c.isalnum() or c == "'":
                word += c.lower()
        flush()
    return tokens


def _written_form(entry: str) -> str:
    """What a vocabulary entry is typed as: the right of ``spoken => written``
    when both sides are present, else the whole entry."""
    spoken, sep, written = entry.strip().partition(_SPOKEN_WRITTEN_SEPARATOR)
    if sep and spoken.strip() and written.strip():
        return written.strip()
    return entry.strip()


def _droppable(run: list[str]) -> str | None:
    if len(run) == 1 and is_negation(run[0]):
        return "isolated-negation"
    if len(run) > _LONGEST_DROP:
        return "span-too-long"
    return None


def _nearer(so_far: str, other: str) -> str:
    return other if so_far == "not-a-subsequence" else so_far


def conservative_edit_violation(
    source: str, output: str, written_words: Sequence[str] = ()
) -> str | None:
    """Why ``output`` is not an acceptable conservative edit of ``source``, or
    None. A port of the product's ``Check::ConservativeEdit`` (DECISIONS
    section 3), kept in step with it by the shared cases in
    ``client/myna-desktop/tests/data/conservative_edit_cases.json``:

    - the output words are an ordered subsequence of the source words, except
      that a word of ``written_words`` (vocabulary entries, ``spoken => written``
      or plain) may stand in for one to three source words that start with its
      first letter, which then count as deleted bar one;
    - at least one word survives;
    - total deletions are at most ``max(2, ceil(n / 2))``;
    - no deleted run, or run a vocabulary word replaces, is longer than 6 words;
    - a lone negation (``not``, ``never``, ``*n't``) is never deleted or
      replaced by itself, though it may go inside a run of two or more.

    The same output can align to the source several ways, so the cheapest
    alignment whose runs are all legal decides.
    """
    heard, answer = _check_words(source), _check_words(output)
    if not answer:
        return "empty"
    written = {w for entry in written_words for w in _check_words(_written_form(entry))}
    n = len(heard)
    # dropped[i][j]: the fewest source words dropped to keep answer[:j] with
    # heard[i - 1] the last one used, or i == 0 before any.
    dropped: list[list[int | None]] = [[None] * (len(answer) + 1) for _ in range(n + 1)]
    dropped[0][0] = 0
    worst = "not-a-subsequence"
    for j, word in enumerate(answer):
        for i in range(n + 1):
            so_far = dropped[i][j]
            if so_far is None:
                continue
            for dropping in range(_LONGEST_DROP + 2):
                nxt = i + dropping
                if nxt >= n:
                    break
                run = heard[i:nxt]
                same = heard[nxt] == word
                if same:
                    spans = range(1, 2)
                elif word in written:
                    spans = range(1, _LONGEST_MISHEARING + 1)
                else:
                    continue
                for span in spans:
                    end = nxt + span
                    if end > n or not word.startswith(heard[nxt][0]):
                        continue
                    swapped = [] if same else heard[nxt:end]
                    why = _droppable(run) or _droppable(swapped)
                    if why:
                        worst = _nearer(worst, why)
                        continue
                    total = so_far + len(run) + max(len(swapped) - 1, 0)
                    best_here = dropped[end][j + 1]
                    dropped[end][j + 1] = total if best_here is None else min(best_here, total)
    budget = max(2, -(-n // 2))
    best: int | None = None
    for i, row in enumerate(dropped):
        so_far = row[len(answer)]
        if so_far is None:
            continue
        why = _droppable(heard[i:])
        if why:
            worst = _nearer(worst, why)
            continue
        total = so_far + (n - i)
        best = total if best is None else min(best, total)
    if best is None:
        return worst
    return None if best <= budget else "too-many-deletions"


def _ngrams(text: str, n: int) -> Counter[str]:
    squashed = "".join(text.split())
    return Counter(squashed[i : i + n] for i in range(len(squashed) - n + 1))


def chrf(
    hypotheses: Sequence[str], references: Sequence[str], order: int = 6, beta: int = 2
) -> float:
    """Corpus chrF on a 0..1 scale: character n-grams 1..``order``, whitespace
    ignored, case kept, ``beta`` weighting recall. Matches sacrebleu's default
    ``CHRF`` (word order 0), which ``tests`` cross-check when it is installed."""
    stats = [[0, 0, 0] for _ in range(order)]
    for hyp, ref in zip(hypotheses, references, strict=True):
        for n in range(1, order + 1):
            h, r = _ngrams(hyp, n), _ngrams(ref, n)
            stats[n - 1][0] += sum(h.values())
            stats[n - 1][1] += sum(r.values())
            stats[n - 1][2] += sum((h & r).values())
    factor = beta**2
    precision = recall = 0.0
    effective = 0
    for n_hyp, n_ref, n_match in stats:
        if n_hyp and n_ref:
            precision += n_match / n_hyp
            recall += n_match / n_ref
            effective += 1
    if not effective:
        return 0.0
    precision, recall = precision / effective, recall / effective
    if not precision + recall:
        return 0.0
    return (1 + factor) * precision * recall / (factor * precision + recall)


def percentile(values: Sequence[float], q: float) -> float:
    """Nearest-rank percentile, ``q`` in 0..100 (empty gives 0)."""
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = max(1, -(-len(ordered) * q // 100))
    return ordered[int(rank) - 1]
