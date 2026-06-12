"""Session contract tests: fake adapter driven by the harness over loopback.

These pin down the semantics every transport and adapter must honour. When
the WebSocket-over-UDS transport lands, it must pass these same tests with
only the client/service wiring swapped.
"""

from collections.abc import AsyncIterator

import pytest

from myna.core import (
    EventSink,
    LoopbackClient,
    PcmChunk,
    SessionConfig,
    TranscriptionFinal,
)
from myna.testbed import FakeAdapter, Harness, ScriptStep, SilenceSource

TERMINAL = ("transcription.done", "transcription.error")


async def run_fake(adapter=None, duration=0.2):
    adapter = adapter or FakeAdapter()
    return await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=SilenceSource(duration_seconds=duration),
    )


async def test_exactly_one_terminal_event_and_it_is_last():
    record = await run_fake()
    kinds = [te.event.type for te in record.events]
    assert sum(k in TERMINAL for k in kinds) == 1
    assert kinds[-1] in TERMINAL


async def test_done_carries_complete_transcript():
    record = await run_fake()
    finals = [te.event.text for te in record.events if te.event.type == "transcription.final"]
    assert record.transcript == " ".join(finals)
    assert record.transcript == "The quick brown fox jumps over the lazy dog."


async def test_event_timestamps_are_monotonic():
    record = await run_fake()
    times = [te.t for te in record.events]
    assert times == sorted(times)


async def test_metrics_populated():
    record = await run_fake()
    m = record.metrics
    assert m.time_to_first_event is not None
    assert m.time_to_first_final is not None
    assert m.time_to_terminal is not None
    assert m.audio_end is not None
    assert m.finalize_latency is not None
    assert m.finalize_latency >= 0
    assert m.time_to_first_event <= m.time_to_first_final <= m.time_to_terminal
    assert m.event_counts["transcription.final"] == 2


async def test_finals_are_never_retracted():
    """The vocabulary has no retraction: every final must survive into done."""
    record = await run_fake()
    for te in record.events:
        if te.event.type == "transcription.final":
            assert te.event.text in record.transcript


class _CrashingAdapter:
    candidate = FakeAdapter().candidate

    async def run_session(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        raise RuntimeError("boom")


async def test_adapter_crash_surfaces_as_error_event():
    record = await run_fake(adapter=_CrashingAdapter())
    kinds = [te.event.type for te in record.events]
    assert kinds == ["transcription.error"]
    assert record.events[0].event.code == "adapter_crash"


async def test_custom_script_immediate_done():
    adapter = FakeAdapter(
        script=(ScriptStep(0.0, TranscriptionFinal(text="hi")),),
        done_after_audio_ends=False,
    )
    record = await run_fake(adapter=adapter)
    assert record.transcript == "hi"


async def test_result_record_serializes_to_json(tmp_path):
    import json

    from myna.testbed.harness import write_records

    record = await run_fake()
    out = tmp_path / "results.jsonl"
    write_records([record], out)
    line = out.read_text().strip()
    parsed = json.loads(line)
    assert parsed["candidate"]["model"] == "fake"
    assert parsed["events"][-1]["event"] in TERMINAL
