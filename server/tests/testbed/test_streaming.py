"""Streaming integration tests (T017-T019, feature 007-streaming-mode).

These verify the streaming contract end-to-end at the harness level using
scripted adapters (no model loads): committed deltas arrive with the
disposition discriminant, segment indices are monotonic, and the committed
concatenation matches the final transcript (the append-only invariant, FR-005).
"""

import pytest

from myna.core import (
    Disposition,
    LoopbackClient,
    SessionConfig,
    TranscriptionDone,
    TranscriptionFinal,
)
from myna.testbed import Harness
from myna.testbed.fake import FakeAdapter, ScriptStep
from myna.testbed.sources import SilenceSource


def streaming_script() -> tuple[ScriptStep, ...]:
    """A scripted streaming session: 3 committed segments then done."""
    return (
        ScriptStep(
            0.0,
            TranscriptionFinal(
                text="Many little wrinkles ",
                disposition=Disposition.COMMITTED,
                segment_index=0,
            ),
        ),
        ScriptStep(
            0.05,
            TranscriptionFinal(
                text="gathered between his eyes ",
                disposition=Disposition.COMMITTED,
                segment_index=1,
            ),
        ),
        ScriptStep(
            0.05,
            TranscriptionFinal(
                text="as he contemplated this.",
                disposition=Disposition.COMMITTED,
                segment_index=2,
            ),
        ),
        ScriptStep(
            0.05,
            TranscriptionDone(
                text="Many little wrinkles gathered between his eyes as he contemplated this."
            ),
        ),
    )


def _source() -> SilenceSource:
    """Short silence clip — the fake adapter doesn't inspect audio."""
    return SilenceSource(duration_seconds=0.2, realtime=False)


# --- T017/T018: streaming emission delivers committed deltas with disposition ---


@pytest.mark.asyncio
async def test_streaming_emits_committed_deltas_with_disposition():
    """Committed deltas carry disposition=committed and monotonic segment_index."""
    service = FakeAdapter(script=streaming_script())
    record = await Harness().run(
        client=LoopbackClient(service),
        candidate=service.candidate,
        source=_source(),
        config=SessionConfig(),
    )

    finals = [te for te in record.events if te.event.type == "transcription.final"]
    assert len(finals) == 3
    for i, te in enumerate(finals):
        assert te.event.disposition == Disposition.COMMITTED
        assert te.event.segment_index == i

    # time_to_first_committed is measured (streaming metric, T002)
    assert record.metrics.time_to_first_committed is not None
    assert record.metrics.committed_segments == 3


# --- T019: committed-text invariant (append-only, FR-005/FR-009) ---


@pytest.mark.asyncio
async def test_committed_concatenation_equals_final():
    """The concatenation of all committed segments equals the final transcript."""
    service = FakeAdapter(script=streaming_script())
    record = await Harness().run(
        client=LoopbackClient(service),
        candidate=service.candidate,
        source=_source(),
        config=SessionConfig(),
    )

    committed = "".join(
        te.event.text
        for te in record.events
        if te.event.type == "transcription.final" and te.event.disposition == Disposition.COMMITTED
    )
    assert committed == record.transcript
    assert record.metrics.commit_stability is True


@pytest.mark.asyncio
async def test_batch_session_is_degenerate_streaming():
    """Batch mode: one committed segment — FR-010 (degenerate streaming case)."""
    service = FakeAdapter()  # default script: 2 finals, no disposition set
    record = await Harness().run(
        client=LoopbackClient(service),
        candidate=service.candidate,
        source=_source(),
        config=SessionConfig(),
    )

    # Absent disposition defaults to committed (backward-compat, FR-004)
    finals = [te for te in record.events if te.event.type == "transcription.final"]
    assert all(te.event.disposition == Disposition.COMMITTED for te in finals)
