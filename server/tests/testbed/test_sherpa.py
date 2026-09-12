"""Sherpa adapter units (008 US4) — event routing over a stub recognizer.

The recognizer itself (sherpa-onnx OnlineRecognizer) is exercised live by
myna-bench bench; here we pin the adapter's disposition routing against a scripted
stub: partials → unstable, endpoints → committed (I1/I2/I4), tail flush (I5),
verbatim-concat spacing (I2), off-format rejection (audio-push invariant).

Batch mode runs the same push loop with the intermediate emissions withheld,
so the "Batch path" section below pins the degenerate shape (I7) and the
regression behind it: an endpoint firing mid-audio must not truncate the
transcript at that endpoint.

The routing tests take unpunctuated adapters: punctuation rewrites committed
text, and asserting routing against rewritten text would make every one of them
an assertion about the punctuation model instead. Where punctuation runs - which
*is* a routing decision, and a different one per emission mode - has its own
section at the end.
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


class StubPunct:
    """Stand-in for ``OnlinePunctuation``: records every text it is handed."""

    def __init__(self):
        self.seen: list[str] = []

    def add_punctuation_with_case(self, text):
        self.seen.append(text)
        return f"<{text}>"


def make_adapter(steps, *, streaming: bool = True, punct=None) -> SherpaAdapter:
    # punctuate=False: the routing suite asserts raw transducer text, and the
    # adapter otherwise picks up a staged punctuation model from the XDG cache
    # - so these would pass or fail on whether a developer had run
    # dev/fetch_sherpa_punct_model.py.
    adapter = SherpaAdapter(streaming=streaming, punctuate=False)
    adapter._recognizer = StubRecognizer(steps)
    if punct is not None:
        adapter._punct = punct
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


# ─── Batch path (I7) ─────────────────────────────────────────────────────────
#
# Same push loop, intermediate emissions withheld: segments accumulate and land
# as one committed final. The regression the accumulation guards against is an
# endpoint firing mid-audio — the transcript must not stop at that endpoint.


@pytest.mark.asyncio
async def test_batch_session_emits_complete_transcript_and_satisfies_i7():
    steps = [
        (False, "he had never been"),
        (True, "he had never been"),
        (False, "father lover husband friend"),
    ]
    events = await run(make_adapter(steps, streaming=False), audio_seconds=2.0)
    assert_batch_degenerate(events)
    assert events[-1].text == "he had never been father lover husband friend"


@pytest.mark.asyncio
async def test_batch_session_withholds_unstable_and_per_segment_commits():
    steps = [(False, "he had"), (True, "he had never been"), (False, "father")]
    events = await run(make_adapter(steps, streaming=False), audio_seconds=2.0)
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [(e.disposition, e.text) for e in finals] == [
        (Disposition.COMMITTED, "he had never been father")
    ]


@pytest.mark.asyncio
async def test_batch_session_empty_audio_emits_empty_done():
    events = await run(make_adapter([], streaming=False), audio_seconds=0.0)
    done = events[-1]
    assert isinstance(done, TranscriptionDone)
    assert done.text == ""


# --- Where punctuation runs (2026-09-08) ------------------------------------
#
# The transducer's vocabulary is 1025 tokens whose only punctuation is an
# apostrophe, so committed text is punctuated by a second model or not at all.
# Punctuation wants a whole sentence; I3/I4 say committed text is never
# restated. The two modes resolve that differently, and both failure modes are
# silent - punctuating per segment in batch would punctuate twice, punctuating
# partials would spend the pass ~20x more often on text about to be replaced.


@pytest.mark.asyncio
async def test_streaming_punctuates_each_commit_and_never_a_partial():
    punct = StubPunct()
    # Trailing empty step so the flush has no tail to re-commit, as in
    # test_streaming_empty_tail_not_committed above.
    steps = [(False, "hello"), (True, "hello world"), (True, "goodbye now"), (False, "")]
    events = await run(make_adapter(steps, punct=punct), audio_seconds=2.0)

    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    committed = [e.text for e in finals if e.disposition == Disposition.COMMITTED]
    unstable = [e.text for e in finals if e.disposition == Disposition.UNSTABLE]

    assert unstable == ["hello"]  # raw: display-only, and replaced on commit
    # Punctuated before I2's synthetic leading space, which is spacing rather
    # than text - the model must not be asked what the whitespace meant.
    assert committed == ["<hello world>", " <goodbye now>"]
    assert punct.seen == ["hello world", "goodbye now"]
    assert events[-1].text == "<hello world> <goodbye now>"


@pytest.mark.asyncio
async def test_batch_punctuates_the_whole_transcript_exactly_once():
    """Nothing commits until the end, so the one pass gets the full context -
    the reason batch recovers the sentence-final marks streaming cannot."""
    punct = StubPunct()
    steps = [(True, "hello world"), (True, "goodbye now"), (False, "")]
    events = await run(make_adapter(steps, streaming=False, punct=punct), audio_seconds=1.5)

    assert punct.seen == ["hello world goodbye now"]  # once, over the join
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [e.text for e in finals] == ["<hello world goodbye now>"]
    assert events[-1].text == "<hello world goodbye now>"


@pytest.mark.asyncio
async def test_unpunctuated_adapter_commits_the_transducer_output_verbatim():
    events = await run(make_adapter([(True, "hello world"), (False, "")]), audio_seconds=1.0)
    assert events[-1].text == "hello world"
