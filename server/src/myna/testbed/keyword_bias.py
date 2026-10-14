"""Contextual biasing toward a session's keywords in a greedy transducer decode.

Each keyword is spelt in the model's own subwords, every segmentation the
vocabulary allows, from a word start (and capitalised, for a sentence start).
The spellings form a character-prefix automaton over token ids, Aho-Corasick
style, so a match survives any segmentation the decoder picks and a broken
match falls back to the longest suffix still matching instead of boosting on.

A token that extends a match earns a bonus per keyword character it spells,
so the one-letter pieces every word could start with earn little and a piece
that commits to the keyword earns most. The bonus only re-ranks non-blank
tokens: the decoder calls `rerank` only when its own choice is not blank.

After the decode, `correct` rewrites a word that is a keyword but for accents
and case to the keyword as written, and an unsure word a near miss of one to
it (murmure's post-correction, gated on the model's confidence).
"""

from __future__ import annotations

import re
import unicodedata
from collections.abc import Sequence

import numpy as np
from numpy.typing import NDArray

# Logit bonus per keyword character a token spells. Tuned on half the frozen
# keyword set (server/.kb/model-evaluation.md has the curve): recall rises
# through 3.0 with no false hits, and past 3.5 false hits and WER climb.
BOOST_PER_CHAR = 3.0

ROOT = 0

# A word the decoder was less sure of than this (its least likely token's
# softmax probability, before any bonus) may be corrected to a keyword it
# nearly spells. Measured 2026-10-13 (server/.kb/model-evaluation.md): false
# hits hold flat up to 0.8 and climb past it.
CORRECT_BELOW_CONFIDENCE = 0.8

# murmure's distances: none under five letters, one edit under eight, two from
# eight. Two edits on a shorter word reach ordinary words.
_CORRECT_MIN_LETTERS = 5
_CORRECT_TWO_EDITS_LETTERS = 8

_WORD = re.compile(r"(\s*)(\W*?)([^\W\d_]+(?:'[^\W\d_]+)*)(\W*)", re.DOTALL)

_SPECIAL = re.compile(r"<.+>")


def _text(keyword: str) -> str:
    """``keyword`` with its whitespace collapsed to single spaces."""
    return " ".join(keyword.split())


def _spellings(keywords: Sequence[str]) -> list[str]:
    return list(
        dict.fromkeys(
            spelling
            for text in map(_text, keywords)
            if text
            for spelling in (" " + text, " " + text[0].upper() + text[1:])
        )
    )


def _segments(spelling: str, pieces: dict[str, int]) -> list[tuple[int, int, int]]:
    """(start, end, token id) for every piece on some complete tokenisation of
    ``spelling``; empty when the vocabulary cannot spell it."""
    n = len(spelling)
    edges = [
        (i, j, pieces[spelling[i:j]])
        for i in range(n)
        for j in range(i + 1, n + 1)
        if spelling[i:j] in pieces
    ]
    reached = {0}
    for i, j, _ in edges:
        if i in reached:
            reached.add(j)
    finishing = {n}
    for i, j, _ in reversed(edges):
        if j in finishing:
            finishing.add(i)
    return [(i, j, t) for i, j, t in edges if i in reached and j in finishing]


def _fold(text: str) -> str:
    decomposed = unicodedata.normalize("NFKD", text)
    return "".join(c for c in decomposed if not unicodedata.combining(c)).casefold()


def _edits(a: str, b: str) -> int:
    previous = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        current = [i]
        for j, y in enumerate(b, 1):
            current.append(min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + (x != y)))
        previous = current
    return previous[-1]


class KeywordBias:
    """A session's keywords as a match automaton over token ids. States are
    ints, `ROOT` is no match in progress; built once per session and shared by
    every decode in it."""

    def __init__(
        self,
        prefixes: list[str],
        children: list[dict[int, int]],
        complete: set[int],
        bonus: NDArray[np.float64],
        spellings: dict[str, str],
    ) -> None:
        self._children = children
        self._spellings = spellings
        self._complete = frozenset(complete)
        self._bonus = bonus
        index = {prefix: state for state, prefix in enumerate(prefixes)}
        # Longest first: the proper suffixes that are match prefixes (so they
        # start a word, as every prefix does), down to the root.
        self._fallback = [
            [index[p[k:]] for k in range(1, len(p) + 1) if p[k:] in index] for p in prefixes
        ]
        # Filled per state as the decode visits it: most states never are.
        self._offers: dict[int, tuple[NDArray[np.intp], NDArray[np.float64]]] = {}

    def complete(self, state: int) -> bool:
        return state in self._complete

    def _offer(self, state: int) -> tuple[NDArray[np.intp], NDArray[np.float64]]:
        """Token ids that extend a match from ``state`` (its own continuations,
        its fallbacks', and every keyword's first piece), with their bonuses."""
        offer = self._offers.get(state)
        if offer is None:
            chain = [state, *self._fallback[state]]
            ids = np.fromiter(
                dict.fromkeys(t for s in chain for t in self._children[s]), dtype=np.intp
            )
            offer = self._offers[state] = (ids, self._bonus[ids])
        return offer

    def rerank(self, state: int, logits: NDArray[np.float32], token: int) -> int:
        """``token`` (the decoder's non-blank argmax over ``logits``), or the
        candidate whose boosted logit strictly beats it."""
        ids, bonus = self._offer(state)
        scores = logits[ids] + bonus
        best = int(np.argmax(scores))
        return int(ids[best]) if scores[best] > logits[token] else token

    def advance(self, state: int, token: int) -> tuple[int, bool]:
        """The state after emitting ``token``, and whether it extended the
        match in progress (rather than falling back or starting over)."""
        child = self._children[state].get(token)
        if child is not None:
            return child, True
        for fallback in self._fallback[state]:
            child = self._children[fallback].get(token)
            if child is not None:
                return child, False
        return ROOT, False

    def correct(
        self, tokens: list[str], timestamps: list[float], confidences: list[float]
    ) -> tuple[list[str], list[float]]:
        """``tokens`` with each corrected word folded into one token, timed
        at its first. ``confidences`` are the tokens' unboosted probabilities."""
        out_tokens: list[str] = []
        out_times: list[float] = []
        start = 0
        while start < len(tokens):
            end = start + 1
            while end < len(tokens) and not tokens[end].startswith(" "):
                end += 1
            word = "".join(tokens[start:end])
            replaced = self._corrected(word, min(confidences[start:end]))
            if replaced is None:
                out_tokens += tokens[start:end]
                out_times += timestamps[start:end]
            else:
                out_tokens.append(replaced)
                out_times.append(timestamps[start])
            start = end
        return out_tokens, out_times

    def _corrected(self, word: str, confidence: float) -> str | None:
        match = _WORD.fullmatch(word)
        if match is None:
            return None
        space, before, core, after = match.groups()
        folded = _fold(core)
        keyword = self._spellings.get(folded)
        if keyword is None and confidence < CORRECT_BELOW_CONFIDENCE:
            keyword = self._nearest(folded)
        if keyword is None or keyword == core:
            return None
        if keyword.islower() and core[0].isupper():
            keyword = keyword[0].upper() + keyword[1:]
        return space + before + keyword + after

    def _nearest(self, folded: str) -> str | None:
        """The one keyword strictly closest to ``folded`` within its reach."""
        if len(folded) < _CORRECT_MIN_LETTERS:
            return None
        reach = 2 if len(folded) >= _CORRECT_TWO_EDITS_LETTERS else 1
        best: list[str] = []
        best_edits = reach
        for spelling, keyword in self._spellings.items():
            edits = _edits(folded, spelling)
            if edits > reach:
                continue
            if edits < best_edits or not best:
                best, best_edits = [keyword], edits
            elif edits == best_edits:
                best.append(keyword)
        return best[0] if len(best) == 1 else None


def keyword_bias(
    keywords: Sequence[str],
    vocab: Sequence[str],
    blank: int,
    *,
    boost_per_char: float = BOOST_PER_CHAR,
) -> KeywordBias | None:
    """The automaton for ``keywords`` over ``vocab`` (token text with the
    word-start marker as a space), or None when none of them can be spelt."""
    pieces: dict[str, int] = {}
    for token_id, piece in enumerate(vocab):
        if piece and token_id != blank and not _SPECIAL.fullmatch(piece):
            pieces.setdefault(piece, token_id)
    prefixes = [""]
    index = {"": ROOT}
    children: list[dict[int, int]] = [{}]
    complete: set[int] = set()

    def state_of(prefix: str) -> int:
        if prefix not in index:
            index[prefix] = len(prefixes)
            prefixes.append(prefix)
            children.append({})
        return index[prefix]

    for spelling in _spellings(keywords):
        segments = _segments(spelling, pieces)
        for i, j, token_id in segments:
            children[state_of(spelling[:i])][token_id] = state_of(spelling[:j])
        if segments:
            complete.add(index[spelling])
    if len(prefixes) == 1:
        return None
    chars = np.array([max(1, len(piece.strip())) for piece in vocab])
    spellings: dict[str, str] = {}
    for keyword in keywords:
        text = _text(keyword)
        if text and " " not in text:
            spellings.setdefault(_fold(text), text)
    return KeywordBias(prefixes, children, complete, boost_per_char * chars, spellings)
