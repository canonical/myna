"""The disfluency tagger: a MiniLM word tagger on ONNX Runtime.

``myna-disfluency-en`` labels every whitespace-separated word KEEP, DELETE,
KEEP_STRIP_COMMA or KEEP_CAPITALIZE. The client runs its own rules first and
vets what this returns (``Check::ConservativeEdit``), so this only tags and
applies the tags. The shape of the model's inputs and the windowing are the
contract its training run fixed (see ``text-snap/README.md``).
"""

from __future__ import annotations

import os
import threading
from collections.abc import Sequence
from pathlib import Path
from typing import Any

import numpy as np

from myna.text.protocol import Model, Params

KEEP, DELETE, STRIP, CAP = 0, 1, 2, 3
STOPS = ".!?"
CLOSERS = "\"')"

#: Up to this many words go through the model in one pass.
SINGLE = 80
#: Longer text goes through in windows of WINDOW words every STRIDE words,
#: each word taking the logits of the window where it is most central.
WINDOW = 64
STRIDE = 40


def decode(words: Sequence[str], tags: Sequence[int]) -> str:
    """Apply ``tags`` to ``words``.

    Beyond the tags: a sentence-final mark on a deleted word moves to the last
    kept word, and a lowercase word that now opens the text or a sentence
    because words before it went takes a capital.
    """
    out: list[str] = []
    pending_stop = ""
    opening = True
    deleted_while_opening = False
    for word, tag in zip(words, tags, strict=True):
        if tag == DELETE:
            tail = word.rstrip(CLOSERS)
            if tail and tail[-1] in STOPS:
                pending_stop = tail[-1]
            if opening:
                deleted_while_opening = True
            continue
        if tag == STRIP and word.endswith(","):
            word = word[:-1]
        elif tag == CAP and word:
            word = word[0].upper() + word[1:]
        if (
            opening
            and deleted_while_opening
            and word
            and word[0].islower()
            and not any(c.isupper() for c in word[1:])
        ):
            word = word[0].upper() + word[1:]
        out.append(word)
        end = word.rstrip(CLOSERS)
        opening = bool(end) and end[-1] in STOPS
        deleted_while_opening = False
    if out and pending_stop:
        last = out[-1].rstrip(",;:")
        if not last or last[-1] not in STOPS + CLOSERS:
            out[-1] = last + pending_stop
    return " ".join(out)


def pick(logits: np.ndarray[Any, Any], tau: float) -> list[int]:
    """The best tag per word, except that a DELETE needs probability ``tau``."""
    e = np.exp(logits - logits.max(-1, keepdims=True))
    p = e / e.sum(-1, keepdims=True)
    tags = p.argmax(-1)
    weak = (tags == DELETE) & (p[:, DELETE] < tau)
    if weak.any():
        rest = p.copy()
        rest[:, DELETE] = -1
        tags = np.where(weak, rest.argmax(-1), tags)
    return [int(t) for t in tags]


class Tagger:
    """Tokenizer, session and windowing around one exported tagger."""

    def __init__(self, session: Any, tokenizer: Any, tau: float) -> None:
        self._session = session
        self._tokenizer = tokenizer
        self._tau = tau

    @classmethod
    def load(cls, model_dir: Path, tau: float, threads: int = 1) -> Tagger:
        import onnxruntime as ort
        from tokenizers import Tokenizer

        options = ort.SessionOptions()
        # Sized explicitly: ORT's own sizing pins threads and is slower.
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        session = ort.InferenceSession(
            os.fspath(model_dir / "tagger.onnx"), options, providers=["CPUExecutionProvider"]
        )
        return cls(session, Tokenizer.from_file(os.fspath(model_dir / "tokenizer.json")), tau)

    def _logits(self, words: Sequence[str]) -> np.ndarray[Any, Any]:
        """Per-word logits, read at each word's first sub-word."""
        encoding = self._tokenizer.encode(list(words), is_pretokenized=True)
        ids = np.array([encoding.ids], dtype=np.int64)
        out = self._session.run(None, {"input_ids": ids, "attention_mask": np.ones_like(ids)})[0][0]
        logits = np.zeros((len(words), out.shape[-1]), dtype=np.float32)
        seen: set[int] = set()
        for position, word in enumerate(encoding.word_ids):
            if word is not None and word not in seen:
                seen.add(word)
                logits[word] = out[position]
        return logits

    def tag(self, words: Sequence[str]) -> list[int]:
        count = len(words)
        if count == 0:
            return []
        if count <= SINGLE:
            return pick(self._logits(words), self._tau)
        best = np.zeros((count, 4), dtype=np.float32)
        centrality = np.full(count, -np.inf)
        start = 0
        while True:
            end = min(count, start + WINDOW)
            logits = self._logits(words[start:end])
            middle = (start + end - 1) / 2
            for k in range(end - start):
                score = -abs(start + k - middle)
                if score > centrality[start + k]:
                    centrality[start + k] = score
                    best[start + k] = logits[k]
            if end == count:
                break
            start += STRIDE
        return pick(best, self._tau)

    def edit(self, text: str) -> str:
        words = text.split()
        return decode(words, self.tag(words)) if words else text


class TaggerModel(Model):
    """The tagger as a ``text.cleanup`` model; loaded on first warm or run."""

    accepts: list[str] = []

    def __init__(self, directory: Path, meta: Params) -> None:
        self._dir = directory
        self.id = str(meta["id"])
        self.task = str(meta["task"])
        self.covers = {"language": [str(meta["language"])]}
        self.latency_ms = int(meta["latency_ms"])
        self.quality = float(meta["quality"])
        self._tau = float(meta["tau"])
        self._tagger: Tagger | None = None
        self._lock = threading.Lock()

    def _loaded(self) -> Tagger:
        if self._tagger is None:
            self._tagger = Tagger.load(self._dir, self._tau)
        return self._tagger

    def warm(self, deadline_s: float) -> None:
        with self._lock:
            self._loaded().edit("Um, hello there.")

    def run(self, text: str, deadline_s: float) -> str:
        with self._lock:
            return self._loaded().edit(text)

    def close(self) -> None:
        with self._lock:
            self._tagger = None
