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
"""

from __future__ import annotations

import re
from collections.abc import Sequence

import numpy as np
from numpy.typing import NDArray

# Logit bonus per keyword character a token spells. Tuned on half the frozen
# keyword set (server/.kb/model-evaluation.md has the curve): recall rises
# through 3.0 with no false hits, and past 3.5 false hits and WER climb.
BOOST_PER_CHAR = 3.0

ROOT = 0

_SPECIAL = re.compile(r"<.+>")


def _spellings(keywords: Sequence[str]) -> list[str]:
    texts = [" ".join(keyword.split()) for keyword in keywords]
    return list(
        dict.fromkeys(
            spelling
            for text in texts
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
    ) -> None:
        self._children = children
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

    def candidates(self, state: int) -> NDArray[np.intp]:
        """Token ids that extend a match from ``state``: its own continuations,
        its fallbacks', and every keyword's first piece."""
        return self._offer(state)[0]

    def _offer(self, state: int) -> tuple[NDArray[np.intp], NDArray[np.float64]]:
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
    return KeywordBias(prefixes, children, complete, boost_per_char * chars)
