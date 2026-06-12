"""Evaluation harness.

The harness is a client: it feeds audio into a session, timestamps every
received event with a monotonic clock, and writes one structured
``ResultRecord`` per run. It speaks only the interfaces in ``myna.core`` —
all model-specific behaviour lives in adapters.

Latency metrics are derived, not measured ad hoc: the raw timed event list is
the source of truth and is preserved in the record so new metrics can be
computed over old runs.
"""

from __future__ import annotations

import json
import time
from collections.abc import Iterable
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

from myna.core import (
    AudioSource,
    SessionConfig,
    SttClient,
    TranscriptionEvent,
    event_to_wire,
)
from myna.testbed.adapter import Candidate


@dataclass(frozen=True)
class TimedEvent:
    """An event with its receive time, seconds since session open."""

    t: float
    event: TranscriptionEvent


@dataclass(frozen=True)
class Metrics:
    """Latencies in seconds since session open, None when not applicable.

    ``finalize_latency`` is the gap between end-of-audio and the terminal
    event — the "time from key release to committed text" number UD129 cares
    about (target: 1–2 s on reference hardware).
    """

    time_to_first_event: float | None
    time_to_first_final: float | None
    time_to_terminal: float | None
    audio_end: float | None
    finalize_latency: float | None
    event_counts: dict[str, int]


@dataclass(frozen=True)
class ResultRecord:
    candidate: Candidate
    config: SessionConfig
    started_at: str  # ISO 8601, UTC
    audio_duration_seconds: float
    events: tuple[TimedEvent, ...]
    audio_end_t: float | None
    metrics: Metrics
    transcript: str

    def to_json(self) -> dict:
        record = asdict(self)
        record["events"] = [
            {"t": te.t, **event_to_wire(te.event)} for te in self.events
        ]
        return record


def compute_metrics(events: Iterable[TimedEvent], audio_end_t: float | None) -> Metrics:
    first = first_final = terminal = None
    counts: dict[str, int] = {}
    for te in events:
        kind = te.event.type
        counts[kind] = counts.get(kind, 0) + 1
        if first is None:
            first = te.t
        if kind == "transcription.final" and first_final is None:
            first_final = te.t
        if kind in ("transcription.done", "transcription.error"):
            terminal = te.t
    finalize = (
        terminal - audio_end_t if terminal is not None and audio_end_t is not None else None
    )
    return Metrics(
        time_to_first_event=first,
        time_to_first_final=first_final,
        time_to_terminal=terminal,
        audio_end=audio_end_t,
        finalize_latency=finalize,
        event_counts=counts,
    )


class Harness:
    """Runs one candidate session and records the result."""

    async def run(
        self,
        client: SttClient,
        candidate: Candidate,
        source: AudioSource,
        config: SessionConfig | None = None,
    ) -> ResultRecord:
        config = config or SessionConfig(audio_format=source.format)
        started_at = datetime.now(timezone.utc).isoformat()
        t0 = time.perf_counter()
        audio_seconds = 0.0
        audio_end_t: float | None = None

        session = await client.open_session(config)
        try:
            timed: list[TimedEvent] = []

            async def feed() -> None:
                nonlocal audio_seconds, audio_end_t
                async for chunk in source.chunks():
                    audio_seconds += chunk.duration_seconds
                    await session.send_audio(chunk)
                await session.finish_audio()
                audio_end_t = time.perf_counter() - t0

            import asyncio

            feeder = asyncio.ensure_future(feed())
            try:
                async for event in session.events():
                    timed.append(TimedEvent(t=time.perf_counter() - t0, event=event))
            finally:
                await feeder
        finally:
            await session.aclose()

        transcript = ""
        for te in timed:
            if te.event.type == "transcription.done":
                transcript = te.event.text  # type: ignore[union-attr]
        return ResultRecord(
            candidate=candidate,
            config=config,
            started_at=started_at,
            audio_duration_seconds=audio_seconds,
            events=tuple(timed),
            audio_end_t=audio_end_t,
            metrics=compute_metrics(timed, audio_end_t),
            transcript=transcript,
        )


def write_records(records: Iterable[ResultRecord], path: Path) -> None:
    """Append result records to a JSONL file, one record per line."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as fp:
        for record in records:
            fp.write(json.dumps(record.to_json()) + "\n")
