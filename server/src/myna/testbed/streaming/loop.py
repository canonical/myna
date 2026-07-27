"""The streaming emission loop (feature 008).

Drives a RollingWindow + StreamingStrategy over a live PCM chunk stream:
re-decode strategies decode the uncommitted window on a cadence; chunked
strategies decode once per detected cut. Emits 007-contract events
(committed finals with monotonic ``segment_index``; unstable finals that
supersede the previous unstable) and returns the accumulated committed
transcript — the caller emits ``TranscriptionDone``.

Invariants enforced here (contracts/emission-semantics.md):
- I1/I2: committed text is append-only; the returned transcript is exactly
  the concatenation of committed emissions.
- I3: unstable emissions never carry a segment_index and never touch
  committed text.
- I4/I5: end-of-audio resolves the outstanding unstable tail — the remainder
  is committed (or dropped if empty) before return.
- I6: RollingWindow bounds the uncommitted buffer (over-cap forces commits).

The ``decode`` callable is injectable so the loop is testable without a model
and reusable across adapters (whisper re-decode, parakeet chunk-commit).
Signature: ``decode(samples: np.ndarray, offset_seconds: float) -> Hypothesis``
with word/segment times in *absolute* session seconds (offset added). It runs
in a worker thread.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Callable

import numpy as np

from myna.core import Disposition, EventSink, PcmChunk, TranscriptionFinal, TranscriptionProgress

from .strategies import Hypothesis, Word
from .window import RollingWindow

MIN_DECODE_S = 0.3  # don't bother decoding sub-300ms tails
_OVERLAP_LOOKBACK = 12  # words of committed history text-alignment dedupe uses


def _norm(text: str) -> str:
    return text.strip().lower().strip(".,!?;:\"'“”")


async def run_streaming_loop(
    audio: AsyncIterator[PcmChunk],
    emit: EventSink,
    decode: Callable[[np.ndarray, float], Hypothesis],
    strategy,
    cadence_seconds: float = 1.0,
    window_cap_seconds: float = 30.0,
    overlap_seconds: float = 1.0,
) -> str:
    window = RollingWindow(window_cap_seconds, overlap_seconds)
    committed: list[str] = []
    committed_through = 0.0  # absolute seconds covered by committed text
    committed_word_texts: list[str] = []  # normalized, for text-alignment dedupe
    segment_index = 0
    last_hyp: Hypothesis | None = None
    last_unstable = ""
    last_decode_end = 0.0

    async def emit_committed(text: str, words: list[Word]) -> None:
        nonlocal segment_index
        await emit(
            TranscriptionFinal(
                text=text,
                disposition=Disposition.COMMITTED,
                segment_index=segment_index,
            )
        )
        committed.append(text)
        committed_word_texts.extend(_norm(w.text) for w in words)
        segment_index += 1

    async def emit_unstable(text: str) -> None:
        nonlocal last_unstable
        if text and text != last_unstable:
            await emit(TranscriptionFinal(text=text, disposition=Disposition.UNSTABLE))
            last_unstable = text

    def fresh_words(words: list[Word]) -> list[Word]:
        """Overlap dedupe (I2): drop words a previous commit already emitted.

        Two signals, unioned: (a) timestamp — word ends at/before the last
        commit's coverage; (b) text alignment — the longest prefix of the new
        words whose normalized texts match a suffix of recently committed
        words. Word timestamps drift a few hundred ms between decodes, so the
        timestamp signal alone leaks boundary duplicates ("USUAL. USUAL.").
        """
        tail = committed_word_texts[-_OVERLAP_LOOKBACK:]
        new = [_norm(w.text) for w in words]
        drop = 0
        max_k = min(len(tail), len(new))
        for k in range(max_k, 0, -1):
            if tail[-k:] == new[:k]:
                drop = k
                break
        return [
            w
            for i, w in enumerate(words)
            if i >= drop and w.end > committed_through + 1e-3
        ]

    async for chunk in audio:
        window.append(chunk.data, chunk.duration_seconds)

        if strategy.mode == "chunked":
            cut = strategy.observe(window.samples(), window.frontier, window.end)
            if cut is not None and cut - window.frontier >= MIN_DECODE_S:
                samples = window.region_before(cut)
                hyp = await asyncio.to_thread(decode, samples, window.frontier)
                fresh = fresh_words(hyp.words)
                text = "".join(w.text for w in fresh).strip()
                if text:
                    await emit_committed(text, fresh)
                    committed_through = max(committed_through, cut)
                window.advance(cut)
            continue

        # re-decode strategies: tick on cadence
        if window.end - last_decode_end < cadence_seconds:
            continue
        last_decode_end = window.end
        hyp = await asyncio.to_thread(decode, window.samples(), window.frontier)
        decision = strategy.commit_rule(last_hyp, hyp, window.end, window.over_cap)
        last_hyp = hyp
        produced = False
        if decision.commit_text and decision.commit_end > committed_through:
            fresh = fresh_words(list(decision.commit_words))
            text = "".join(w.text for w in fresh).strip() if fresh else ""
            if not text and not decision.commit_words:
                text = decision.commit_text  # strategy gave no words; trust it
            if text:
                await emit_committed(text, fresh)
                committed_through = decision.commit_end
                window.advance(decision.commit_end)
                last_unstable = ""  # I4: commit clears unstable
                produced = True
        before = last_unstable
        await emit_unstable(decision.unstable_text)
        produced = produced or last_unstable != before
        if not produced:
            await emit(TranscriptionProgress())  # liveness on quiet ticks

    # I5: resolve the tail — decode whatever is left and commit it.
    if window.window_seconds >= MIN_DECODE_S:
        hyp = await asyncio.to_thread(decode, window.samples(), window.frontier)
        fresh = fresh_words(hyp.words)
        tail = "".join(w.text for w in fresh).strip()
        if tail:
            await emit_committed(tail, fresh)
    return " ".join(committed)
