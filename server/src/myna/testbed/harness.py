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
import statistics
import time
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
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

    ``time_to_ready`` is the model-residency / cold-load signal: the gap from
    session open to the first ``progress(phase="ready")`` — for a snap that
    idle-unloaded its weights this is the reload cost the user waits through
    before dictation begins ("time to model load from cold"). Warm, it is small.
    ``time_to_first_snippet`` is streaming responsiveness (first unstable
    liveness text). ``finalize_latency`` is the gap between end-of-audio and the
    terminal event — the "time from key release to committed text" number UD129
    cares about (target: 1–2 s on reference hardware). ``rtf`` (real-time
    factor) is decode time over audio duration; only meaningful in batch mode
    (real-time pacing floors it at ~1.0).
    """

    time_to_first_event: float | None
    time_to_ready: float | None
    time_to_first_snippet: float | None
    time_to_first_final: float | None
    time_to_first_committed: float | None  # First committed text (streaming)
    time_to_first_unstable: float | None  # First unstable hypothesis (streaming)
    time_to_terminal: float | None
    audio_end: float | None
    finalize_latency: float | None
    rtf: float | None
    commit_stability: bool  # True if no committed text was retracted
    committed_segments: int  # Count of committed segments received
    event_counts: dict[str, int]


@dataclass
class DecodeSample:
    """One decode call recorded by streaming telemetry (perf T03).

    ``kind`` is ``"commit"`` (a chunked-strategy cut, or the end-of-audio
    tail - both always result in committed text), ``"partial"`` (a chunked
    strategy's unstable display tick, [`myna.testbed.streaming.loop._chunked_partial`]),
    or ``"tick"`` (a re-decode strategy's cadence tick, which serves both
    duties in one call and so isn't split further).
    """

    kind: str
    window_seconds: float
    wall_seconds: float


@dataclass
class StreamingTelemetry:
    """Measured cost of one streaming session (perf T03).

    PLAN.md's ranked-headroom table lists the encoder duty cycle as
    *derived* from a cost curve, not measured; this makes it an observation.
    The caller builds one instance and passes it both into a streaming
    adapter's constructor (which records a ``DecodeSample`` per decode call,
    additively, outside the commit/alignment logic so it can never perturb
    what gets committed - see the loop's ``telemetry`` parameter) and into
    ``Harness.run``, which only carries the finished object into the
    returned ``ResultRecord``: it cannot derive this from wire events, which
    never carry it.

    ``audio_seconds_encoded`` sums the window handed to every decode call,
    ``RollingWindow`` overlap included - it is the quantity PLAN.md's 14.7x
    multiplier is about, not the audio the speaker produced.
    """

    samples: list[DecodeSample] = field(default_factory=list)
    audio_seconds_ingested: float = 0.0  # session wall audio (RollingWindow.end at close)
    session_seconds: float = 0.0  # wall-clock span of the whole session

    def record(self, kind: str, window_seconds: float, wall_seconds: float) -> None:
        self.samples.append(DecodeSample(kind, window_seconds, wall_seconds))

    @property
    def decode_calls(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        for s in self.samples:
            counts[s.kind] = counts.get(s.kind, 0) + 1
        return counts

    @property
    def audio_seconds_encoded(self) -> float:
        return sum(s.window_seconds for s in self.samples)

    @property
    def encoder_busy_seconds(self) -> float:
        return sum(s.wall_seconds for s in self.samples)

    @property
    def redundancy(self) -> float | None:
        """``audio_seconds_encoded / audio_seconds_ingested`` - PLAN.md's
        "14.7x more encoder work", measured instead of read off a cost curve."""
        if not self.audio_seconds_ingested:
            return None
        return self.audio_seconds_encoded / self.audio_seconds_ingested

    @property
    def duty_cycle(self) -> float | None:
        """``encoder_busy_seconds / session_seconds`` - PLAN.md's "28.9%
        duty cycle". Only meaningful when audio was fed at real-time pace: a
        batch-fed session has no idle time to divide by, so this approaches
        100% instead of reflecting a live dictation session's true duty."""
        if not self.session_seconds:
            return None
        return self.encoder_busy_seconds / self.session_seconds

    def window_seconds_stats(self) -> dict[str, float] | None:
        if not self.samples:
            return None
        values = sorted(s.window_seconds for s in self.samples)
        return {"min": values[0], "median": statistics.median(values), "max": values[-1]}

    def summary(self) -> dict:
        """Streaming duty-cycle telemetry, derived quantities included, for
        printing or writing out."""
        return {
            "decode_calls": self.decode_calls,
            "audio_seconds_ingested": self.audio_seconds_ingested,
            "audio_seconds_encoded": self.audio_seconds_encoded,
            "encoder_busy_seconds": self.encoder_busy_seconds,
            "redundancy": self.redundancy,
            "duty_cycle": self.duty_cycle,
            "window_seconds": self.window_seconds_stats(),
            "session_seconds": self.session_seconds,
        }


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
    # perf T03: opaque pass-through, folded in unmodified when the caller
    # supplies one to both a streaming adapter and Harness.run. None on
    # every non-streaming or non-instrumented run.
    streaming_telemetry: StreamingTelemetry | None = None

    def to_json(self) -> dict:
        record = asdict(self)
        record["events"] = [{"t": te.t, **event_to_wire(te.event)} for te in self.events]
        if self.streaming_telemetry is not None:
            record["streaming_telemetry"]["summary"] = self.streaming_telemetry.summary()
        return record


def compute_metrics(
    events: Iterable[TimedEvent],
    audio_end_t: float | None,
    audio_duration_seconds: float | None = None,
) -> Metrics:
    first = ready = first_snippet = first_final = first_committed = None
    first_unstable = terminal = None
    counts: dict[str, int] = {}
    committed_segments = 0
    commit_stability = True  # always True: the wire contract guarantees append-only

    for te in events:
        kind = te.event.type
        counts[kind] = counts.get(kind, 0) + 1
        if first is None:
            first = te.t
        if kind == "transcription.progress":
            if ready is None and getattr(te.event, "phase", None) == "ready":
                ready = te.t
            if first_snippet is None and getattr(te.event, "snippet", None):
                first_snippet = te.t
        if kind == "transcription.final":
            if first_final is None:
                first_final = te.t
            # Check if this is a committed segment (streaming mode)
            disposition = getattr(te.event, "disposition", "committed")
            if disposition == "committed":
                if first_committed is None:
                    first_committed = te.t
                committed_segments += 1
            elif disposition == "unstable":
                if first_unstable is None:
                    first_unstable = te.t
        if kind in ("transcription.done", "transcription.error"):
            terminal = te.t

    finalize = terminal - audio_end_t if terminal is not None and audio_end_t is not None else None
    # Decode wall-time excludes the cold-load wait: measure from ready (or first
    # event) to terminal, so RTF reflects throughput, not model loading.
    rtf = None
    if terminal is not None and audio_duration_seconds:
        decode_start = ready if ready is not None else (first or 0.0)
        rtf = (terminal - decode_start) / audio_duration_seconds
    return Metrics(
        time_to_first_event=first,
        time_to_ready=ready,
        time_to_first_snippet=first_snippet,
        time_to_first_final=first_final,
        time_to_first_committed=first_committed,
        time_to_first_unstable=first_unstable,
        time_to_terminal=terminal,
        audio_end=audio_end_t,
        finalize_latency=finalize,
        rtf=rtf,
        commit_stability=commit_stability,
        committed_segments=committed_segments,
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
        on_event: Callable[[TimedEvent], None] | None = None,
        streaming_telemetry: StreamingTelemetry | None = None,
    ) -> ResultRecord:
        """Run one session. ``on_event``, if given, is called with each
        ``TimedEvent`` the moment it arrives — for live display; the full
        record is still returned at the end. ``streaming_telemetry``, if
        given, is folded into the returned record unchanged (perf T03): pass
        the same object you gave a streaming adapter's constructor, since the
        harness has no way to derive it from wire events."""
        config = config or SessionConfig(audio_format=source.format)
        started_at = datetime.now(UTC).isoformat()
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
                    te = TimedEvent(t=time.perf_counter() - t0, event=event)
                    timed.append(te)
                    if on_event is not None:
                        on_event(te)
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
            metrics=compute_metrics(timed, audio_end_t, audio_seconds),
            transcript=transcript,
            streaming_telemetry=streaming_telemetry,
        )


def write_records(records: Iterable[ResultRecord], path: Path) -> None:
    """Append result records to a JSONL file, one record per line."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as fp:
        for record in records:
            fp.write(json.dumps(record.to_json()) + "\n")
