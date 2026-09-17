"""Bounded batch inference with deferred presentation (audio review A5).

Batch means the transcript is shown once, after the audio ends - not that the
whole utterance is decoded in one pass. A toggle session has no length cap, so
holding every sample until release grows without bound. Instead the chunked
loop cuts the utterance at the first pause past ``BATCH_ARM_S`` (or forcibly at
``BATCH_FORCE_CUT_S``), decodes each region once and hands its committed text
to the adapter, which presents everything at the end.

An utterance shorter than the arm point still decodes whole, exactly as
before. A pause cut keeps no overlap (its trailing silence means no word
straddles it); a forced cut keeps ``BATCH_OVERLAP_S`` and the loop
deduplicates the words re-decoded from it.
"""

from __future__ import annotations

from collections.abc import AsyncIterator, Awaitable, Callable

import numpy as np
from numpy.typing import NDArray

from myna.core import EventSink, PcmChunk, TranscriptionProgress

from .loop import run_streaming_loop
from .strategies import SC_FORCE_CUT_S, SC_SILENCE_CUT_S, Hypothesis, SilenceCut, Word

BATCH_ARM_S = 30.0
BATCH_FORCE_CUT_S = SC_FORCE_CUT_S
BATCH_WINDOW_CAP_S = BATCH_FORCE_CUT_S + 5.0
BATCH_OVERLAP_S = 1.0
_PROGRESS_INTERVAL_S = 1.0


async def run_deferred_batch(
    audio: AsyncIterator[PcmChunk],
    emit: EventSink,
    decode: Callable[[NDArray[np.float32], float], Hypothesis],
    on_commit: Callable[[str, list[Word]], Awaitable[None]],
) -> None:
    """Drive ``decode`` over bounded regions of ``audio``. Only progress
    reaches ``emit``; committed text goes to ``on_commit`` in order."""

    async def progress_only(event: object) -> None:
        if isinstance(event, TranscriptionProgress):
            await emit(event)

    await run_streaming_loop(
        audio,
        progress_only,
        decode,
        SilenceCut(
            arm_seconds=BATCH_ARM_S,
            silence_cut_seconds=SC_SILENCE_CUT_S,
            force_cut_seconds=BATCH_FORCE_CUT_S,
        ),
        cadence_seconds=_PROGRESS_INTERVAL_S,
        window_cap_seconds=BATCH_WINDOW_CAP_S,
        overlap_seconds=BATCH_OVERLAP_S,
        on_commit=on_commit,
        silence_cut_overlap=False,
        min_utterance_seconds=0.0,
    )
