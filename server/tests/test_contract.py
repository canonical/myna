"""Session contract tests: fake adapter driven by the harness.

These pin down the semantics every transport and adapter must honour. Each
test runs against every transport (loopback and WebSocket-over-UDS) with only
the client/service wiring swapped — the loopback run is the reference
semantics, the others prove the wire protocol preserves them.
"""

import contextlib
from collections.abc import AsyncIterator

import pytest

from myna.core import (
    EventSink,
    LoopbackClient,
    PcmChunk,
    SessionConfig,
    TranscriptionError,
    TranscriptionFinal,
    WsUnixClient,
    serve_unix,
)
from myna.testbed import FakeAdapter, Harness, ScriptStep, SilenceSource

TERMINAL = ("transcription.done", "transcription.error")


@contextlib.asynccontextmanager
async def loopback_transport(service, tmp_path):
    yield LoopbackClient(service)


@contextlib.asynccontextmanager
async def ws_unix_transport(service, tmp_path):
    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(service, socket_path):
        yield WsUnixClient(socket_path)


@pytest.fixture(params=[loopback_transport, ws_unix_transport], ids=["loopback", "ws-uds"])
def transport(request):
    return request.param


@pytest.fixture
def run_fake(transport, tmp_path):
    async def run(adapter=None, duration=0.2):
        adapter = adapter or FakeAdapter()
        async with transport(adapter, tmp_path) as client:
            return await Harness().run(
                client=client,
                candidate=adapter.candidate,
                source=SilenceSource(duration_seconds=duration),
            )

    return run


async def test_exactly_one_terminal_event_and_it_is_last(run_fake):
    record = await run_fake()
    kinds = [te.event.type for te in record.events]
    assert sum(k in TERMINAL for k in kinds) == 1
    assert kinds[-1] in TERMINAL


async def test_done_carries_complete_transcript(run_fake):
    record = await run_fake()
    finals = [te.event.text for te in record.events if te.event.type == "transcription.final"]
    assert record.transcript == " ".join(finals)
    assert record.transcript == "The quick brown fox jumps over the lazy dog."


async def test_event_timestamps_are_monotonic(run_fake):
    record = await run_fake()
    times = [te.t for te in record.events]
    assert times == sorted(times)


async def test_metrics_populated(run_fake):
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
    # Residency/streaming liveness extracted from the phase transitions the
    # fake adapter drives (preparing -> ready -> snippet): time_to_ready is the
    # cold-load signal, time_to_first_snippet the streaming-responsiveness one.
    assert m.time_to_ready is not None
    assert m.time_to_first_snippet is not None
    assert m.time_to_first_event <= m.time_to_ready <= m.time_to_first_snippet
    assert m.rtf is not None and m.rtf >= 0


async def test_finals_are_never_retracted(run_fake):
    """The vocabulary has no retraction: every final must survive into done."""
    record = await run_fake()
    for te in record.events:
        if te.event.type == "transcription.final":
            assert te.event.text in record.transcript


class _CrashingAdapter:
    candidate = FakeAdapter().candidate

    def capabilities(self):
        return FakeAdapter().capabilities()

    async def run_session(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        raise RuntimeError("boom")


async def test_adapter_crash_surfaces_as_error_event(run_fake):
    record = await run_fake(adapter=_CrashingAdapter())
    kinds = [te.event.type for te in record.events]
    assert kinds == ["transcription.error"]
    assert record.events[0].event.code == "adapter_crash"


class _ImmediateErrorAdapter:
    """Emits a terminal error before consuming any audio — like an adapter that
    fails to load its model. Reproduces the mid-stream-close case: the server
    closes the connection while the client feeder is still pushing audio."""

    candidate = FakeAdapter().candidate

    def capabilities(self):
        return FakeAdapter().capabilities()

    async def run_session(
        self, config: SessionConfig, audio: AsyncIterator[PcmChunk], emit: EventSink
    ) -> None:
        await emit(TranscriptionError(code="inference_failed", message="model load failed"))


async def test_terminal_error_mid_stream_does_not_mask_the_error(transport, tmp_path):
    """A terminal error emitted while audio is still streaming must surface as the
    error event, not a ConnectionClosed traceback from the still-running feeder
    (regression: send_audio into a server-closed socket used to raise)."""
    adapter = _ImmediateErrorAdapter()
    async with transport(adapter, tmp_path) as client:
        record = await Harness().run(
            client=client,
            candidate=adapter.candidate,
            source=SilenceSource(duration_seconds=1.0, realtime=True),
        )
    assert [te.event.type for te in record.events] == ["transcription.error"]
    assert record.events[0].event.code == "inference_failed"
    assert record.events[0].event.message == "model load failed"


async def test_custom_script_immediate_done(run_fake):
    adapter = FakeAdapter(
        script=(ScriptStep(0.0, TranscriptionFinal(text="hi")),),
        done_after_audio_ends=False,
    )
    record = await run_fake(adapter=adapter)
    assert record.transcript == "hi"


async def test_capabilities_query_round_trips_over_transport(transport, tmp_path):
    """capabilities() must survive the wire identically to the in-process call —
    the same contract guarantee the event stream gets."""
    adapter = FakeAdapter()
    async with transport(adapter, tmp_path) as client:
        caps = await client.capabilities()
    assert caps == adapter.capabilities()
    assert caps.models == ("fake",)
    assert caps.input_formats  # non-empty: clients need a format to deliver


async def test_result_record_serializes_to_json(run_fake, tmp_path):
    import json

    from myna.testbed.harness import write_records

    record = await run_fake()
    out = tmp_path / "results.jsonl"
    write_records([record], out)
    line = out.read_text().strip()
    parsed = json.loads(line)
    assert parsed["candidate"]["model"] == "fake"
    assert parsed["events"][-1]["event"] in TERMINAL
