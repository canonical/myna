"""Candidate metadata for the faster-whisper adapter (no model/extra needed).

Constructing the adapter and reading ``.candidate`` does not import
faster_whisper, so this runs in the default offline suite. It guards the
label normalisation that keeps result records readable when the snap loads
weights from a local CTranslate2 model-component directory (T15).
"""

import asyncio

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
