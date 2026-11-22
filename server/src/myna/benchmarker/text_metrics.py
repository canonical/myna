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


def conservative_edit_violation(
    source: str, output: str, written_words: Sequence[str] = ()
) -> str | None:
    """Why ``output`` is not an acceptable conservative edit of ``source``, or
    None. This is the model-agnostic ``Check::ConservativeEdit`` of DECISIONS
    section 3, in its reference form:

    - the output words are an ordered subsequence of the source words (a word
      listed in ``written_words`` may expand: it is matched case-insensitively
      anywhere in the output and is not counted as an insertion);
    - at least one word survives when the source had one;
    - total deletions are at most ``max(2, n / 2)``;
    - no deleted run is longer than 6 words;
    - a lone negation (``not``, ``never``, ``*n't``) is never deleted by itself,
      though it may go inside a run of two or more.
    """
    src, out = words(source), words(output)
    allowed = {w.casefold() for w in written_words}
    out = [w for w in out if w not in allowed]
    if len(out) > len(src):
        return "not-a-subsequence"
    deleted = len(src) - len(out)
    if src and not out:
        return "empty"
    if deleted > max(2, len(src) // 2):
        return "too-many-deletions"
    # The same output can align to the source several ways ("not on Monday,
    # no, on Tuesday" -> "on Tuesday" keeps either "on"); the edit is
    # acceptable if any alignment has only legal deleted runs.
    return _alignment_violation(src, out)


def _legal_run(run: list[str]) -> bool:
    return len(run) <= 6 and not (len(run) == 1 and is_negation(run[0]))


def _alignment_violation(src: list[str], out: list[str]) -> str | None:
    @cache
    def ok(i: int, j: int, start: int) -> bool:
        """Can src[i:] align to out[j:], given a deleted run began at ``start``
        (or -1 when the previous word was kept)?"""
        if i == len(src):
            return j == len(out) and (start < 0 or _legal_run(src[start:i]))
        if j < len(out) and src[i] == out[j] and (start < 0 or _legal_run(src[start:i])):
            if ok(i + 1, j + 1, -1):
                return True
        return ok(i + 1, j, i if start < 0 else start)

    if ok(0, 0, -1):
        return None
    if not _is_subsequence(src, out):
        return "not-a-subsequence"
    spans = _greedy_runs(src, out)
    if any(len(span) > 6 for span in spans):
        return "span-too-long"
    return "isolated-negation"


def _is_subsequence(src: list[str], out: list[str]) -> bool:
    kept = iter(src)
    return all(token in kept for token in out)


def _greedy_runs(src: list[str], out: list[str]) -> list[list[str]]:
    runs: list[list[str]] = []
    j = 0
    in_run = False
    for token in src:
        if j < len(out) and token == out[j]:
            j += 1
            in_run = False
            continue
        if not in_run:
            runs.append([])
            in_run = True
        runs[-1].append(token)
    return runs


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
