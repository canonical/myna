"""Whisper batch-session units - model-free.

The real adapter is covered end-to-end by tests/test_whisper_adapter.py, which
needs the ``whisper`` extra and the tiny weights and so skips on a stock
checkout. That left the emission contract itself untested: what run_session
puts on the wire for a given decode. Following the Parakeet units' house
pattern, a stub stands in for ``WhisperModel`` so the segment -> final
mapping, the I2 verbatim concatenation, the readiness ordering and the
failure path are pinned without loading weights.
"""

from __future__ import annotations

import pytest

pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import (
    PHASE_READY,
    AudioFormat,
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.whisper import FasterWhisperAdapter

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


class _Segment:
    """One faster-whisper segment (the attributes the adapter reads)."""

    def __init__(self, text, start=0.0, end=1.0, avg_logprob=-0.1):
        self.text = text
        self.start = start
        self.end = end
        self.avg_logprob = avg_logprob


class _FakeWhisperModel:
    """Stands in for ``faster_whisper.WhisperModel``: yields scripted segments
    instead of decoding. ``transcribe`` returns a generator, as the real one
    does, so the adapter's drain-in-the-worker-thread step is exercised."""

    def __init__(self, *segments):
        self._segments = segments
        self.calls: list[dict] = []

    def transcribe(self, samples, **kwargs):
        self.calls.append({"samples": len(samples), **kwargs})
        return (seg for seg in self._segments), None


def adapter_with(*segments) -> FasterWhisperAdapter:
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _FakeWhisperModel(*segments)  # skips the lazy load
    return adapter


async def pcm_audio(seconds: float, chunk_s: float = 0.5):
    for _ in range(int(seconds / chunk_s)):
        yield PcmChunk(data=b"\x01\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run_session(adapter, audio_seconds: float = 1.0, fmt=FORMAT, **config):
    events = []

    async def emit(event):
        events.append(event)

    cfg = SessionConfig(audio_format=fmt, language="en-GB", **config)
    await adapter.run_session(cfg, pcm_audio(audio_seconds), emit)
    return events


def finals(events):
    return [e for e in events if isinstance(e, TranscriptionFinal)]


async def test_each_segment_commits_and_done_is_their_verbatim_concatenation():
    adapter = adapter_with(_Segment(" hello there"), _Segment(" world"))

    events = await run_session(adapter)

    # I2: only the first final sheds its leading space, so concatenating the
    # deltas verbatim reproduces the transcript - no separator is inserted.
    assert [f.text for f in finals(events)] == ["hello there", " world"]
    assert all(f.disposition is Disposition.COMMITTED for f in finals(events))
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == "hello there world"


async def test_blank_segments_are_dropped():
    adapter = adapter_with(_Segment("   "), _Segment(" real speech "), _Segment(""))

    events = await run_session(adapter)

    assert [f.text for f in finals(events)] == ["real speech"]
    assert events[-1].text == "real speech"


async def test_ready_is_signalled_before_any_audio_is_pulled():
    """The client gates on ready; pulling audio first deadlocks the session
    (docs/architecture/ie115-lifecycle.md 3A)."""
    adapter = adapter_with(_Segment(" hi"))
    events = []
    phases_at_first_pull = []

    async def emit(event):
        events.append(event)

    async def audio():
        phases_at_first_pull.extend(e.phase for e in events if isinstance(e, TranscriptionProgress))
        yield PcmChunk(data=b"\x01\x00" * 16_000, format=FORMAT)

    await adapter.run_session(SessionConfig(audio_format=FORMAT), audio(), emit)

    assert PHASE_READY in phases_at_first_pull


async def test_silence_is_finalised_without_troubling_the_model():
    adapter = adapter_with(_Segment(" never decoded"))

    events = await run_session(adapter, audio_seconds=0.0)

    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""
    assert adapter._model.calls == []


async def test_progress_ticks_while_audio_is_buffering():
    adapter = adapter_with(_Segment(" hi"))

    events = await run_session(adapter, audio_seconds=3.0)

    # readiness plus a heartbeat per second of buffered audio (interval 1.0 s)
    buffering = [
        e for e in events if isinstance(e, TranscriptionProgress) and e.phase != PHASE_READY
    ]
    assert len(buffering) >= 2


async def test_timestamps_are_attached_only_when_asked_for():
    adapter = adapter_with(_Segment(" hi", start=0.25, end=1.5, avg_logprob=-0.3))

    without = await run_session(adapter)
    assert finals(without)[0].segments == ()

    adapter = adapter_with(_Segment(" hi", start=0.25, end=1.5, avg_logprob=-0.3))
    with_stamps = await run_session(adapter, timestamp_granularity="segment")
    segment = finals(with_stamps)[0].segments[0]
    assert (segment.start, segment.end, segment.score) == (0.25, 1.5, -0.3)
    assert segment.text == "hi"


async def test_region_subtags_are_dropped_for_the_decoder():
    """faster-whisper rejects "en-GB"; the adapter passes bare ISO 639-1."""
    adapter = adapter_with(_Segment(" hi"))

    await run_session(adapter)

    assert adapter._model.calls[0]["language"] == "en"


async def test_a_failed_decode_is_reported_as_an_error_not_a_crash():
    class _Boom:
        def transcribe(self, samples, **kwargs):
            raise RuntimeError("ct2 exploded")

    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _Boom()

    events = await run_session(adapter)

    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "RuntimeError: ct2 exploded" in events[-1].message


def test_a_local_model_directory_is_labelled_by_its_leaf():
    """Snap model components arrive as absolute paths; result records would be
    unreadable if the candidate carried the whole path."""
    adapter = FasterWhisperAdapter("/snap/myna/current/models/faster-whisper-small/")

    assert adapter.candidate.model == "whisper-faster-whisper-small"


def test_english_only_checkpoints_advertise_english_only():
    assert FasterWhisperAdapter("tiny.en").capabilities().languages == ("en",)
    assert FasterWhisperAdapter("tiny").capabilities().languages == ("*",)


def test_streaming_switches_the_advertised_strategy():
    assert FasterWhisperAdapter("tiny").candidate.streaming_strategy == "commit-on-finalize"
    streaming = FasterWhisperAdapter("tiny", streaming=True)
    assert streaming.streaming and streaming.candidate.streaming_strategy == "local-agreement"


async def test_unload_releases_the_model_and_is_idempotent():
    adapter = adapter_with(_Segment(" hi"))

    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idle-unload may fire again before a new session
    assert adapter._model is None


async def test_streaming_re_decodes_the_window_with_word_timestamps():
    """The local-agreement strategy compares successive hypotheses word by
    word, so the streaming decode must ask for word timestamps and hand back
    the whole window each tick. (The strategy itself is covered by
    test_emission_invariants; this pins whisper's wiring into it.)"""

    class _Word:
        def __init__(self, word, start, end):
            self.word, self.start, self.end = word, start, end

    class _WordSegment(_Segment):
        def __init__(self, text, words):
            super().__init__(text)
            self.words = words

    class _StreamingModel:
        def __init__(self):
            self.window_seconds: list[float] = []

        def transcribe(self, samples, **kwargs):
            assert kwargs["word_timestamps"] is True
            self.window_seconds.append(len(samples) / 16_000)
            return (s for s in [_WordSegment(" hi", [_Word(" hi", 0.0, 0.4)])]), None

    adapter = FasterWhisperAdapter("tiny", streaming=True, stream_cadence_s=0.5)
    adapter._model = _StreamingModel()

    events = await run_session(adapter, audio_seconds=2.0)

    assert isinstance(events[-1], TranscriptionDone)
    assert adapter._model.window_seconds  # the decoder was actually driven
    assert adapter._model.window_seconds == sorted(adapter._model.window_seconds)


async def test_a_format_mismatch_is_refused_before_the_model_is_touched():
    adapter = adapter_with(_Segment(" hi"))

    events = await run_session(adapter, fmt=AudioFormat(sample_rate_hz=44_100))

    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"
    assert adapter._model.calls == []
