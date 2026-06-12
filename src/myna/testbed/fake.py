"""Fake adapter: emits scripted events with configurable timing.

No model, no audio inspection. Permanent regression fixture — it pins down
the session contract (event ordering, terminal-event guarantees, timing
observability) so transport and harness changes can be verified without GPUs
or audio hardware. Do not delete it when real adapters land.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Sequence
from dataclasses import dataclass

from myna.core import (
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionEvent,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate


@dataclass(frozen=True)
class ScriptStep:
    """Emit ``event`` after sleeping ``delay_seconds`` from the previous step."""

    delay_seconds: float
    event: TranscriptionEvent


def default_script() -> tuple[ScriptStep, ...]:
    """A small two-segment session exercising every non-error event type."""
    return (
        ScriptStep(0.05, TranscriptionProgress(snippet="the quick")),
        ScriptStep(0.05, TranscriptionProgress(snippet="the quick brown")),
        ScriptStep(0.05, TranscriptionFinal(text="The quick brown fox")),
        ScriptStep(0.05, TranscriptionProgress(snippet="jumps over")),
        ScriptStep(0.05, TranscriptionFinal(text="jumps over the lazy dog.")),
    )


class FakeAdapter:
    """Plays a script of events on a timeline, draining audio concurrently.

    If the script does not end with a terminal event, a ``transcription.done``
    aggregating all final texts is emitted after the script finishes — after
    end-of-audio if ``done_after_audio_ends`` (the default, mimicking a real
    engine that flushes on end of input).
    """

    def __init__(
        self,
        script: Sequence[ScriptStep] | None = None,
        *,
        done_after_audio_ends: bool = True,
    ) -> None:
        self._script = tuple(script) if script is not None else default_script()
        self._done_after_audio_ends = done_after_audio_ends

    @property
    def candidate(self) -> Candidate:
        return Candidate(model="fake", engine="scripted", streaming_strategy="scripted")

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        audio_done = asyncio.Event()

        async def drain_audio() -> None:
            async for _ in audio:
                pass
            audio_done.set()

        drain = asyncio.ensure_future(drain_audio())
        try:
            finals: list[str] = []
            terminal_emitted = False
            for step in self._script:
                await asyncio.sleep(step.delay_seconds)
                await emit(step.event)
                if isinstance(step.event, TranscriptionFinal):
                    finals.append(step.event.text)
                terminal_emitted = step.event.type in (
                    "transcription.done",
                    "transcription.error",
                )
            if not terminal_emitted:
                if self._done_after_audio_ends:
                    await audio_done.wait()
                await emit(TranscriptionDone(text=" ".join(finals)))
        finally:
            await drain
