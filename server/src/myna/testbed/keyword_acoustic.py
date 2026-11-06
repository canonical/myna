"""Keyword correction scored against the audio, for a CTC model.

No pronunciation dictionary: every run of the greedy tokens holding n-1 to n+1
letters of an n-letter keyword is a candidate, its audio the frames between
the neighbouring tokens. The keyword replaces the run when its CTC
log-likelihood there comes within ``MARGIN_PER_LETTER`` nats per keyword
letter of the run's own; best margin first, no overlaps. Han, kana and Hangul
keywords only: letters, not tokens, are counted because Korean pieces span
syllables. A Hangul syllable the vocabulary lacks is scored as each
in-vocabulary syllable one jamo from it, since the model can never emit it.

Measured on SenseVoice-Small (server/.kb/model-evaluation.md): FLEURS
keyword recall 27.7 -> 80.7% (zh), 18.5 -> 75.9% (ko), 31.6 -> 84.2% (ja) on
the holdout halves, no ordinary false hits for zh or ko.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from functools import lru_cache

import numpy as np
from numpy.typing import NDArray

# Tuned on the tune halves of the three FLEURS keyword tiers: 4 is where
# ordinary false hits start.
MARGIN_PER_LETTER = 3.0

LETTER = re.compile("[぀-ヺー-ヿ㐀-䶿一-鿿豈-﫿가-힯]")

_HANGUL_BASE, _HANGUL_COUNT = 0xAC00, 11172
_VARIANTS = 200


@dataclass(frozen=True)
class CtcToken:
    """One token of the greedy decode: its id, its text (a leading space for
    a word start) and the first and last frame of its run."""

    id: int
    piece: str
    first: int
    last: int


def greedy(log_probs: NDArray[np.float32], blank: int) -> list[tuple[int, int, int, float]]:
    """(id, first frame, last frame, peak probability) per emitted token."""
    path = np.argmax(log_probs, axis=-1)
    out: list[tuple[int, int, int, float]] = []
    prev = blank
    for t, token in enumerate(path.tolist()):
        if token != blank and token == prev:
            tid, first, _, peak = out[-1]
            out[-1] = (tid, first, t, max(peak, float(np.exp(log_probs[t, token]))))
        elif token != blank:
            out.append((token, t, t, float(np.exp(log_probs[t, token]))))
        prev = token
    return out


def ctc_log_likelihood(
    log_probs: NDArray[np.float32], a: int, b: int, labels: Sequence[int], blank: int = 0
) -> float:
    """log P(``labels`` | frames ``a``..``b``) under CTC."""
    frames = b - a + 1
    if frames <= 0 or len(labels) > frames:
        return -np.inf
    ext = [blank]
    for label in labels:
        ext += [label, blank]
    ext_ids = np.array(ext)
    skip = np.zeros(len(ext), bool)
    skip[2:] = (ext_ids[2:] != blank) & (ext_ids[2:] != ext_ids[:-2])
    alpha = np.full(len(ext), -np.inf)
    emit = log_probs[a, ext_ids].astype(np.float64)
    alpha[0] = emit[0]
    if len(ext) > 1:
        alpha[1] = emit[1]
    for t in range(a + 1, b + 1):
        emit = log_probs[t, ext_ids].astype(np.float64)
        one = np.concatenate(([-np.inf], alpha[:-1]))
        two = np.where(skip, np.concatenate(([-np.inf, -np.inf], alpha[:-2])), -np.inf)
        alpha = np.logaddexp(np.logaddexp(alpha, one), two) + emit
    tail = alpha[-1] if len(ext) == 1 else np.logaddexp(alpha[-1], alpha[-2])
    return float(tail)


def _hangul_neighbours(syllable: str) -> list[str]:
    offset = ord(syllable) - _HANGUL_BASE
    if not 0 <= offset < _HANGUL_COUNT:
        return []
    initial, medial, final = offset // 588, (offset % 588) // 28, offset % 28
    near = [chr(_HANGUL_BASE + initial * 588 + medial * 28 + f) for f in range(28) if f != final]
    near += [chr(_HANGUL_BASE + initial * 588 + m * 28 + final) for m in range(21) if m != medial]
    return near


class CtcKeywords:
    """A session's Han, kana and Hangul keywords spelt in one model's tokens.
    ``encode`` gives a text's token ids, or None when the vocabulary cannot
    spell it."""

    def __init__(
        self,
        keywords: Sequence[str],
        encode: Callable[[str], tuple[int, ...] | None],
        blank: int = 0,
        margin: float = MARGIN_PER_LETTER,
    ) -> None:
        self._encode = lru_cache(maxsize=None)(encode)
        self._blank = blank
        self._margin = margin
        self.keywords = tuple(
            dict.fromkeys(
                k
                for k in (" ".join(k.split()) for k in keywords)
                if len(k) >= 2 and all(LETTER.fullmatch(ch) for ch in k)
            )
        )
        self._spellings = {k: self._spell(k) for k in self.keywords}

    def __bool__(self) -> bool:
        return any(self._spellings.values())

    def _spell(self, keyword: str) -> list[tuple[int, ...]]:
        """Token sequences to score for ``keyword``: itself, or with each
        syllable the vocabulary lacks swapped for its neighbours."""
        whole = self._encode(keyword)
        if whole is not None:
            return [whole]
        variants = [""]
        for ch in keyword:
            options = [ch] if self._encode(ch) is not None else []
            options = options or [n for n in _hangul_neighbours(ch) if self._encode(n) is not None]
            if not options:
                return []
            variants = [v + o for v in variants for o in options][:_VARIANTS]
        return [ids for v in variants if (ids := self._encode(v)) is not None]

    def corrections(
        self, tokens: Sequence[CtcToken], log_probs: NDArray[np.float32]
    ) -> list[tuple[int, int, str]]:
        """(first token, end token, keyword) for each run to rewrite."""
        frames = log_probs.shape[0]
        ids = sorted({i for seqs in self._spellings.values() for seq in seqs for i in seq})
        if not ids or not tokens:
            return []
        column = {token: j for j, token in enumerate(ids)}
        # Exact pruning: P(keyword | a..b) is at most the product of each of
        # its tokens' probability summed over a..b, and the run's own
        # likelihood at least its greedy path's, so a window whose bound
        # misses the margin cannot pass the full score.
        mass = np.vstack(
            [np.zeros((1, len(ids))), np.cumsum(np.exp(log_probs[:, ids], dtype=np.float64), 0)]
        )
        best_path = np.concatenate(([0.0], np.cumsum(log_probs.max(axis=1), dtype=np.float64)))
        own: dict[tuple[int, int], float] = {}
        found: list[tuple[float, int, int, str]] = []
        for keyword, spellings in self._spellings.items():
            n = len(keyword)
            for i, end in _runs(tokens, n):
                text = "".join(t.piece for t in tokens[i:end]).strip()
                if text == keyword:
                    continue
                a = tokens[i - 1].last + 1 if i else 0
                b = tokens[end].first - 1 if end < len(tokens) else frames - 1
                if b < a:
                    continue
                greedy_ll = best_path[b + 1] - best_path[a]
                cols = [[column[t] for t in seq] for seq in spellings]
                bound = max(
                    float(np.log(np.maximum(mass[b + 1, c] - mass[a, c], 1e-300)).sum())
                    for c in cols
                )
                if bound - greedy_ll <= -self._margin * n:
                    continue
                if (i, end) not in own:
                    labels = [t.id for t in tokens[i:end]]
                    own[(i, end)] = ctc_log_likelihood(log_probs, a, b, labels, self._blank)
                score = max(
                    ctc_log_likelihood(log_probs, a, b, seq, self._blank) for seq in spellings
                )
                margin = (score - own[(i, end)]) / n
                if margin > -self._margin:
                    found.append((margin, i, end, keyword))
        taken = [False] * len(tokens)
        out: list[tuple[int, int, str]] = []
        for _, i, end, keyword in sorted(found, reverse=True):
            if any(taken[i:end]):
                continue
            out.append((i, end, keyword))
            for x in range(i, end):
                taken[x] = True
        return sorted(out)


def _runs(tokens: Sequence[CtcToken], n: int) -> list[tuple[int, int]]:
    """(first, end) of every run of letter-only tokens holding n-1..n+1 letters."""
    out: list[tuple[int, int]] = []
    for i in range(len(tokens)):
        count = 0
        for j in range(i, len(tokens)):
            piece = tokens[j].piece.strip()
            if not piece or not all(LETTER.fullmatch(ch) for ch in piece):
                break
            count += len(piece)
            if count > n + 1:
                break
            if count >= n - 1:
                out.append((i, j + 1))
    return out


def tdt_log_likelihood(
    logits: NDArray[np.float32], labels: Sequence[int], vocab_size: int, blank: int
) -> float:
    """log P(``labels`` | a window) under a TDT joint's [frames, labels+1,
    vocab+durations] logits: a label at (t, u) moves t by its duration (0
    allowed), a blank by 1 or more; every label emitted at or past the
    window's end counts."""
    frames = logits.shape[0]
    tok = logits[..., :vocab_size].astype(np.float64)
    tok -= np.logaddexp.reduce(tok, axis=-1, keepdims=True)
    dur = logits[..., vocab_size:].astype(np.float64)
    dur -= np.logaddexp.reduce(dur, axis=-1, keepdims=True)
    durations = dur.shape[-1]
    n = len(labels)
    alpha = np.full((frames + durations, n + 1), -np.inf)
    alpha[0, 0] = 0.0
    for t in range(frames):
        for u in range(n + 1):
            here = alpha[t, u]
            if here == -np.inf:
                continue
            stay = here + tok[t, u, blank]
            for d in range(1, durations):
                alpha[t + d, u] = np.logaddexp(alpha[t + d, u], stay + dur[t, u, d])
            if u < n:
                emit = here + tok[t, u, labels[u]]
                for d in range(durations):
                    alpha[t + d, u + 1] = np.logaddexp(alpha[t + d, u + 1], emit + dur[t, u, d])
    return float(np.logaddexp.reduce(alpha[frames:, n]))
