"""Offline unit tests for the Nemotron adapter's own logic (T09).

These need neither NeMo, a GPU, nor the model — they exercise the code that
lives in the *adapter* rather than the model: audio-format rejection, the
result-shape unwrapping that absorbs NeMo's API drift, idle-unload, the
cold-load heartbeat, and the buffer→final→done finalisation. The real model
load (``_load_blocking``) is left to the hardware integration test
(``test_nemotron_adapter.py``); mocking NeMo here would assert call shapes,
not behaviour.

Two invariants are checked with Hypothesis (the Python QuickCheck) rather than
hand-picked examples, because they're claims over a wide input space:
  * *any* non-canonical audio format is rejected (the "symmetric across
    rate/channels/width" promise in ``run_session``), and
  * the transcript is recovered identically from every NeMo result shape.
"""

import asyncio

from hypothesis import assume, given
from hypothesis import strategies as st

from myna.core import (
    PHASE_PREPARING,
    PHASE_TRANSCRIBING,
    AudioFormat,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed import nemotron as nemotron_mod
from myna.testbed.nemotron import (
    DEFAULT_MODEL,
    NEMO_FORMAT,
    NEMO_RATE,
    NemotronAdapter,
)

CANONICAL = (NEMO_RATE, 1, 2)  # 16 kHz mono S16LE — the only accepted format


# --- helpers ---------------------------------------------------------------


async def _drive_session(adapter, config, chunks):
    """Run a session to completion, returning the emitted events in order."""
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for chunk in chunks:
            yield chunk

    await adapter.run_session(config, audio(), emit)
    return events


def _pcm_seconds(seconds: float, fmt: AudioFormat = NEMO_FORMAT) -> PcmChunk:
    """A silent PCM chunk of the given real-time duration."""
    return PcmChunk(data=b"\x00\x00" * int(seconds * fmt.sample_rate_hz), format=fmt)


class _Hypothesis:
    """Newer NeMo returns objects with a ``.text`` attribute, not bare strings."""

    def __init__(self, text):
        self.text = text


# --- audio-format rejection (property) -------------------------------------


@given(
    rate=st.integers(min_value=1, max_value=192_000),
    channels=st.integers(min_value=1, max_value=8),
    width=st.integers(min_value=1, max_value=4),
)
def test_rejects_any_noncanonical_format(rate, channels, width):
    # The rejection happens before the model loads, so no stub is needed.
    assume((rate, channels, width) != CANONICAL)
    fmt = AudioFormat(sample_rate_hz=rate, channels=channels, sample_width_bytes=width)
    events = asyncio.run(
        _drive_session(NemotronAdapter(), SessionConfig(audio_format=fmt), [])
    )
    assert len(events) == 1  # rejected outright, nothing else emitted
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


async def test_canonical_format_is_accepted(monkeypatch):
    # The mirror of the property: the one good format is *not* rejected.
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    events = await _drive_session(adapter, SessionConfig(audio_format=NEMO_FORMAT), [])
    assert not any(isinstance(e, TranscriptionError) for e in events)


# --- result-shape unwrapping (property) ------------------------------------


@given(text=st.text())
def test_unwrap_recovers_text_from_every_nemo_result_shape(text):
    shapes = [
        [text],  # older NeMo: list[str]
        [_Hypothesis(text)],  # newer NeMo: list[Hypothesis]
        ([text], [text]),  # hybrid model: (best, all) of str
        ([_Hypothesis(text)], [_Hypothesis(text)]),  # hybrid: (best, all) of Hypothesis
    ]
    for shape in shapes:
        # the raw text is recovered verbatim; stripping is run_session's job
        assert nemotron_mod._unwrap_transcript(shape) == text


# --- capabilities ----------------------------------------------------------


def test_capabilities_describe_english_punctuating_model():
    caps = NemotronAdapter(DEFAULT_MODEL).capabilities()
    assert caps.models == ("stt_en_fastconformer_hybrid_large_streaming_multi",)
    assert caps.languages == ("en",)
    assert caps.punctuation is True
    assert caps.translation is False
    assert NEMO_FORMAT in caps.input_formats


def test_capabilities_label_strips_local_checkpoint_path():
    # the shared _label helper, reached via capabilities rather than candidate
    caps = NemotronAdapter("/snap/nemotron/components/9/model/foo.nemo").capabilities()
    assert caps.models == ("foo",)


# --- idle-unload -----------------------------------------------------------


async def test_unload_drops_model_and_is_idempotent():
    # idle-unload (T27); no real model needed — just the reference handling
    adapter = NemotronAdapter()
    adapter._model = object()
    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idempotent, must not raise


async def test_load_model_reuses_cached_instance():
    # warm path: an already-loaded model is returned without touching the
    # (hardware-only) loader — the memoise half of the load/unload pair.
    adapter = NemotronAdapter()
    sentinel = object()
    adapter._model = sentinel
    assert await adapter._load_model() is sentinel


# --- cold-load heartbeat ---------------------------------------------------


def _instant(value):
    async def _load():
        return value

    return _load


async def _collect_heartbeat(adapter):
    events = []

    async def emit(event):
        events.append(event)

    await adapter._load_model_with_heartbeat(emit)
    return events


async def test_heartbeat_ticks_during_slow_load(monkeypatch):
    monkeypatch.setattr(nemotron_mod, "_LOAD_HEARTBEAT_SECONDS", 0.02)
    adapter = NemotronAdapter()

    async def slow_load():
        await asyncio.sleep(0.1)  # ~5 heartbeat intervals
        return object()

    monkeypatch.setattr(adapter, "_load_model", slow_load)
    events = await _collect_heartbeat(adapter)

    assert all(isinstance(e, TranscriptionProgress) for e in events)
    assert all(e.phase == PHASE_PREPARING for e in events)  # "loading…", not transcribing
    assert len(events) >= 3  # immediate tick + several while loading


async def test_heartbeat_emits_once_when_warm(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    events = await _collect_heartbeat(adapter)
    # just the immediate liveness tick, tagged as the loading phase
    assert events == [TranscriptionProgress(phase=PHASE_PREPARING)]


# --- run_session finalisation ---------------------------------------------


async def test_empty_audio_finalises_with_empty_done(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    events = await _drive_session(adapter, SessionConfig(), [])

    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""
    assert not any(isinstance(e, TranscriptionFinal) for e in events)


async def test_buffered_audio_becomes_one_final_then_done(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    # stub the blocking decode; run_session owns the surrounding contract
    monkeypatch.setattr(adapter, "_transcribe", lambda model, pcm: "  Hello world.  ")

    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])

    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [e.text for e in finals] == ["Hello world."]  # stripped to one final
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == "Hello world."


async def test_whitespace_only_transcript_emits_no_final(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    monkeypatch.setattr(adapter, "_transcribe", lambda model, pcm: "   \n  ")

    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])

    assert not any(isinstance(e, TranscriptionFinal) for e in events)
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""


async def test_progress_ticks_once_per_second_of_buffered_audio(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    monkeypatch.setattr(adapter, "_transcribe", lambda model, pcm: "ok")

    # three one-second chunks -> three transcribing-phase liveness ticks
    chunks = [_pcm_seconds(1.0) for _ in range(3)]
    events = await _drive_session(adapter, SessionConfig(), chunks)

    transcribing = [
        e
        for e in events
        if isinstance(e, TranscriptionProgress) and e.phase == PHASE_TRANSCRIBING
    ]
    assert len(transcribing) == 3


async def test_decode_failure_surfaces_as_error_event(monkeypatch):
    adapter = NemotronAdapter()
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))

    def boom(model, pcm):
        raise RuntimeError("CUDA out of memory")

    monkeypatch.setattr(adapter, "_transcribe", boom)
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])

    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "RuntimeError" in events[-1].message
