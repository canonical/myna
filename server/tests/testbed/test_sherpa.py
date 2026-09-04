"""Sherpa adapter units (008 US4) — event routing over a stub recognizer.

The recognizer itself (sherpa-onnx OnlineRecognizer) is exercised live by
dev/bench.py; here we pin the adapter's disposition routing against a scripted
stub: partials → unstable, endpoints → committed (I1/I2/I4), tail flush (I5),
verbatim-concat spacing (I2), off-format rejection (audio-push invariant).

Batch path tests (``_decode_oneshot``) are in the "Batch path" section below.
The critical regression: endpoint fires mid-audio in batch mode → old code
returned only the pre-endpoint text, silently dropping the tail segment.
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from test_emission_invariants import assert_batch_degenerate

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
        e
        for e in events
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


# ─── Batch path ──────────────────────────────────────────────────────────────
#
# _decode_oneshot loops over endpoint boundaries and accumulates segment texts.
# The regression (pre-fix): the ``while is_ready`` loop exits at the first
# endpoint, so only the pre-endpoint text was returned — the tail was silently
# dropped.  These stubs exercise that path without loading the ONNX model.


class _BatchStream:
    """Minimal stream stub for _decode_oneshot: each accept_waveform / input_finished
    triggers one decode cycle on the parent recognizer."""

    def __init__(self, recognizer):
        self._rec = recognizer

    def accept_waveform(self, rate, samples):
        self._rec._ready = True

    def input_finished(self):
        self._rec._ready = True


class _BatchRecognizer:
    """Scripted OnlineRecognizer for _decode_oneshot multi-segment tests.

    ``segments`` is a list of ``(text, fires_endpoint)`` tuples.  Each element
    represents one fully-decoded cycle: when the loop calls ``decode_stream``,
    readiness is consumed; when an endpoint fires and ``reset`` is called, the
    recognizer advances to the next segment and becomes ready again.
    """

    def __init__(self, segments):
        self._segs = list(segments)
        self._idx = 0
        self._ready = False

    def create_stream(self):
        return _BatchStream(self)

    def is_ready(self, stream):
        return self._ready and self._idx < len(self._segs)

    def decode_stream(self, stream):
        self._ready = False

    def is_endpoint(self, stream):
        return self._idx < len(self._segs) and self._segs[self._idx][1]

    def get_result(self, stream):
        return self._segs[self._idx][0] if self._idx < len(self._segs) else ""

    def reset(self, stream):
        self._idx += 1
        if self._idx < len(self._segs):
            self._ready = True


def make_batch_adapter(segments) -> SherpaAdapter:
    adapter = SherpaAdapter(streaming=False)
    adapter._recognizer = _BatchRecognizer(segments)
    return adapter


# ── Unit tests for _decode_oneshot ───────────────────────────────────────────


def test_decode_oneshot_single_segment_no_endpoint():
    rec = _BatchRecognizer([("hello world", False)])
    stream = rec.create_stream()
    result = SherpaAdapter._decode_oneshot(rec, stream, np.zeros(16_000, dtype=np.float32))
    assert result == "hello world"


def test_decode_oneshot_accumulates_across_endpoint_regression():
    """Regression: endpoint fires mid-audio in batch mode.

    Before the fix, ``_decode_oneshot`` exited the ``while is_ready`` loop on
    the first endpoint and returned only ``"he had never been"`` — the tail
    segment was silently dropped.  After the fix, both segments are accumulated
    and the full transcript is returned.
    """
    rec = _BatchRecognizer([("he had never been", True), ("father lover husband friend", False)])
    stream = rec.create_stream()
    result = SherpaAdapter._decode_oneshot(rec, stream, np.zeros(16_000, dtype=np.float32))
    assert result == "he had never been father lover husband friend"


def test_decode_oneshot_multiple_endpoint_segments():
    rec = _BatchRecognizer([("one", True), ("two", True), ("three", False)])
    stream = rec.create_stream()
    result = SherpaAdapter._decode_oneshot(rec, stream, np.zeros(16_000, dtype=np.float32))
    assert result == "one two three"


def test_decode_oneshot_empty_endpoint_segment_excluded():
    # A silence-induced endpoint producing empty text must not inject a
    # spurious space into the concatenated result.
    rec = _BatchRecognizer([("", True), ("hello", False)])
    stream = rec.create_stream()
    result = SherpaAdapter._decode_oneshot(rec, stream, np.zeros(16_000, dtype=np.float32))
    assert result == "hello"


def test_decode_oneshot_all_empty_returns_empty_string():
    rec = _BatchRecognizer([("", True), ("", False)])
    stream = rec.create_stream()
    result = SherpaAdapter._decode_oneshot(rec, stream, np.zeros(16_000, dtype=np.float32))
    assert result == ""


# ── Session-level batch test (I7) ────────────────────────────────────────────


@pytest.mark.asyncio
async def test_batch_session_emits_complete_transcript_and_satisfies_i7():
    """I7: batch mode = one committed segment, equal to the full transcript.

    Also exercises the run_session dispatch path end-to-end (load → buffer
    → _decode_oneshot → emit committed + done) with an endpoint that fires
    mid-audio — the regression that triggered the batch fix.
    """
    adapter = make_batch_adapter(
        [("he had never been", True), ("father lover husband friend", False)]
    )
    events = await run(adapter, audio_seconds=2.0)
    assert_batch_degenerate(events)
    done = events[-1]
    assert done.text == "he had never been father lover husband friend"


@pytest.mark.asyncio
async def test_batch_session_empty_audio_emits_empty_done():
    adapter = make_batch_adapter([])
    events = await run(adapter, audio_seconds=0.0)
    done = events[-1]
    assert isinstance(done, TranscriptionDone)
    assert done.text == ""
