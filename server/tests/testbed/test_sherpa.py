"""Sherpa adapter units (008 US4) — event routing over a stub recognizer.

The recognizer itself (sherpa-onnx OnlineRecognizer) is exercised live by
dev/bench.py; here we pin the adapter's disposition routing against a scripted
stub: partials → unstable, endpoints → committed (I1/I2/I4), tail flush (I5),
verbatim-concat spacing (I2), off-format rejection (audio-push invariant).
"""

from __future__ import annotations

import numpy as np
import pytest

from myna.core import (
    AudioFormat,
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionFinal,
)
from myna.testbed.sherpa import SherpaAdapter

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


class StubStream:
    def __init__(self, recognizer):
        self.recognizer = recognizer

    def accept_waveform(self, rate, samples):
        self.recognizer.pushes.append(len(samples))
        self.recognizer._ready = True  # new audio decodes once
        # The result evolves per push (partials are not consumed); the flush
        # re-reads the last push's result, like the real recognizer.
        self.recognizer._current = min(self.recognizer._current + 1, len(self.recognizer.steps) - 1)

    def input_finished(self):
        self.recognizer.finished = True
        self.recognizer._ready = True


class StubRecognizer:
    """Scripted OnlineRecognizer: steps[i] is the (endpoint, text) result
    after the i-th push. Each push becomes ready once; decode_stream consumes
    readiness (the real recognizer drains buffered frames the same way —
    without this, the adapter's decode loop would spin forever)."""

    def __init__(self, steps):
        self.steps = list(steps)
        self.pushes: list[int] = []
        self.finished = False
        self.resets = 0
        self._ready = False
        self._current = -1

    def create_stream(self):
        return StubStream(self)

    def is_ready(self, stream):
        return self._ready

    def decode_stream(self, stream):
        self._ready = False

    def is_endpoint(self, stream):
        return bool(0 <= self._current < len(self.steps) and self.steps[self._current][0])

    def get_result(self, stream):
        return self.steps[self._current][1] if 0 <= self._current < len(self.steps) else ""

    def reset(self, stream):
        self.resets += 1


def make_adapter(steps) -> SherpaAdapter:
    adapter = SherpaAdapter(streaming=True)
    adapter._recognizer = StubRecognizer(steps)
    return adapter


async def pcm_audio(seconds: float, chunk_s: float = 0.5):
    for _ in range(int(seconds / chunk_s)):
        yield PcmChunk(data=b"\x01\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run(adapter, audio_seconds=2.0, fmt=FORMAT):
    events = []

    async def emit(e):
        events.append(e)

    cfg = SessionConfig(audio_format=fmt, language="en")
    await adapter.run_session(cfg, pcm_audio(audio_seconds), emit)
    return events


@pytest.mark.asyncio
async def test_streaming_routes_partial_endpoint_and_tail():
    # push 1: partial; push 2: endpoint commits it; pushes 3-4: partial grows;
    # end-of-audio: flush commits the tail (I5).
    steps = [
        (False, "hello"),
        (True, "hello world"),
        (False, "goodbye"),
        (False, "goodbye now"),
    ]
    # The flush re-reads the current step's text (no steps left after reset? —
    # endpoint reset pops; the flush reads the remaining partial).
    events = await run(make_adapter(steps), audio_seconds=2.0)

    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    committed = [e for e in finals if e.disposition == Disposition.COMMITTED]
    unstable = [e for e in finals if e.disposition == Disposition.UNSTABLE]
    done = events[-1]
    assert isinstance(done, TranscriptionDone)

    # I1: monotonic indices; I2: verbatim concat (synthetic leading space on
    # the second segment); I5: the tail was resolved before done.
    assert [e.segment_index for e in committed] == [0, 1]
    assert [e.text for e in committed] == ["hello world", " goodbye now"]
    assert done.text == "hello world goodbye now"
    # I3: unstable never carries an index; I4: no stale unstable after commit.
    assert all(e.segment_index is None for e in unstable)
    assert unstable[0].text == "hello"


@pytest.mark.asyncio
async def test_streaming_empty_tail_not_committed():
    steps = [(True, "only segment"), (False, "")]
    events = await run(make_adapter(steps), audio_seconds=1.0)
    committed = [
        e for e in events
        if isinstance(e, TranscriptionFinal) and e.disposition == Disposition.COMMITTED
    ]
    assert len(committed) == 1
    assert events[-1].text == "only segment"


@pytest.mark.asyncio
async def test_off_format_audio_rejected():
    bad = AudioFormat(sample_rate_hz=48_000, channels=1, sample_width_bytes=2)
    events = await run(make_adapter([]), fmt=bad)
    assert type(events[0]).__name__ == "TranscriptionError"
    assert events[0].code == "unsupported_audio_format"
