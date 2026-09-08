"""Candidate metadata for the faster-whisper adapter (no model/extra needed).

Constructing the adapter and reading ``.candidate`` does not import
faster_whisper, so this runs in the default offline suite. It guards the
label normalisation that keeps result records readable when the snap loads
weights from a local CTranslate2 model-component directory (T15).
"""

import asyncio
import sys

import pytest
from hypothesis import assume, given
from hypothesis import strategies as st

from myna.core import AudioFormat, SessionConfig, TranscriptionError
from myna.testbed.whisper import WHISPER_RATE, FasterWhisperAdapter, _iso639_1

CANONICAL = (WHISPER_RATE, 1, 2)  # 16 kHz mono S16LE — the only accepted format


def test_iso639_1_drops_region_subtag():
    # faster-whisper rejects BCP-47 region tags; the corpus uses them.
    assert _iso639_1("en-GB") == "en"
    assert _iso639_1("de") == "de"
    assert _iso639_1(None) is None  # auto-detect


def test_candidate_labels_a_bare_size():
    cand = FasterWhisperAdapter("small").candidate
    assert cand.model == "whisper-small"
    assert cand.engine == "faster-whisper-cpu"
    assert cand.streaming_strategy == "commit-on-finalize"


async def test_unload_drops_the_model():
    # idle-unload (T27); no real model needed — just the reference handling
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = object()
    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idempotent


def test_candidate_labels_a_component_directory_by_leaf():
    # snap passes --model $SNAP_COMPONENTS/model-small
    cand = FasterWhisperAdapter("/snap/whisper/components/42/model-small/", device="cuda").candidate
    assert cand.model == "whisper-model-small"  # leaf, not the absolute path
    assert cand.engine == "faster-whisper-cuda"


@given(
    rate=st.integers(min_value=1, max_value=192_000),
    channels=st.integers(min_value=1, max_value=8),
    width=st.integers(min_value=1, max_value=4),
)
def test_rejects_any_noncanonical_format(rate, channels, width):
    # Rejection happens before the model loads (audio-push: client owns
    # capture + conversion), so this needs neither faster-whisper nor a model.
    assume((rate, channels, width) != CANONICAL)
    fmt = AudioFormat(sample_rate_hz=rate, channels=channels, sample_width_bytes=width)

    async def drive():
        events = []

        async def emit(event):
            events.append(event)

        async def no_audio():
            for chunk in ():  # rejected before any chunk is read
                yield chunk

        await FasterWhisperAdapter("tiny").run_session(
            SessionConfig(audio_format=fmt), no_audio(), emit
        )
        return events

    events = asyncio.run(drive())
    assert len(events) == 1  # rejected outright, nothing else emitted
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


# ─── compute type ────────────────────────────────────────────────────────────


def _fake_ctranslate2(monkeypatch, supported):
    """CTranslate2's own answer, stubbed - the real one needs the whisper extra."""
    module = type(
        "M", (), {"get_supported_compute_types": staticmethod(lambda device: set(supported))}
    )
    monkeypatch.setitem(sys.modules, "ctranslate2", module)


def test_a_compute_type_the_device_cannot_do_is_refused(monkeypatch):
    """CTranslate2 rejects float16-on-CPU too, but only from inside the model
    constructor and without naming what it would have taken - which is a sweep
    cell dying on its first clip, or a daemon that starts and then fails on
    first dictation."""
    _fake_ctranslate2(monkeypatch, {"float32", "int8", "int8_float32"})
    adapter = FasterWhisperAdapter("tiny", device="cpu", compute_type="float16")
    with pytest.raises(ValueError, match="not available on device 'cpu'"):
        adapter._check_compute_type()


def test_a_supported_compute_type_passes(monkeypatch):
    _fake_ctranslate2(monkeypatch, {"float32", "int8", "int8_float32"})
    FasterWhisperAdapter("tiny", device="cpu", compute_type="int8")._check_compute_type()


@pytest.mark.parametrize("deferral", ["default", "auto"])
def test_a_deferral_is_not_a_request_and_is_left_to_ctranslate2(monkeypatch, deferral):
    def explode(device):
        raise AssertionError("a deferral must not be checked against the supported set")

    monkeypatch.setitem(
        sys.modules,
        "ctranslate2",
        type("M", (), {"get_supported_compute_types": staticmethod(explode)}),
    )
    FasterWhisperAdapter("tiny", compute_type=deferral)._check_compute_type()
