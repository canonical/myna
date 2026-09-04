"""Integration test for the faster-whisper adapter (T07).

Needs the ``whisper`` extra and the tiny model (downloaded on first run,
cached under HF_HOME). Skips cleanly when either is unavailable, so the
default offline test run stays green.

Transcription quality assertions are deliberately loose: the fixtures are
synthetic espeak speech and the model is whisper-tiny — this verifies the
contract and the plumbing, not WER (that's T06/T25 territory).
"""

from pathlib import Path

import pytest

pytest.importorskip("faster_whisper", reason="install with: uv sync --extra whisper")

from myna.core import LoopbackClient, SessionConfig, WsUnixClient, serve_unix
from myna.testbed import Harness, load_manifest
from myna.testbed.whisper import FasterWhisperAdapter

MANIFEST = Path(__file__).parent.parent / "fixtures" / "manifest.json"

pytestmark = pytest.mark.skipif(
    not MANIFEST.exists(), reason="run `python dev/generate_fixtures.py` from repo root first"
)


@pytest.fixture
def adapter():
    try:
        from faster_whisper import WhisperModel

        WhisperModel("tiny", device="cpu")
    except Exception as exc:  # no network and no cached model
        pytest.skip(f"whisper-tiny model unavailable: {exc}")
    return FasterWhisperAdapter("tiny")


async def test_whisper_tiny_transcribes_fixture_clip(adapter):
    clips = {clip.id: clip for clip in load_manifest(MANIFEST)}
    clip = clips["quiet-weather"]
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=clip.open_source(),
        config=SessionConfig(audio_format=clip.open_source().format, language="en"),
    )

    kinds = [te.event.type for te in record.events]
    assert kinds[-1] == "transcription.done"
    assert kinds.count("transcription.done") == 1
    assert "transcription.progress" in kinds  # activity signal while buffering
    assert "transcription.error" not in kinds

    # commit-on-finalize: every final must come after all audio was sent
    audio_end = record.audio_end_t
    finals = [te for te in record.events if te.event.type == "transcription.final"]
    assert finals and all(te.t >= audio_end for te in finals)
    assert record.transcript == " ".join(te.event.text for te in finals)

    # loose accuracy floor for tiny + synthetic speech
    assert "afternoon" in record.transcript.lower()


async def test_whisper_over_ws_unix_transport(adapter, tmp_path):
    clips = {clip.id: clip for clip in load_manifest(MANIFEST)}
    clip = clips["quiet-pangram"]
    async with serve_unix(adapter, tmp_path / "ubustt.sock"):
        record = await Harness().run(
            client=WsUnixClient(tmp_path / "ubustt.sock"),
            candidate=adapter.candidate,
            source=clip.open_source(realtime=True),  # paced like live dictation
            config=SessionConfig(audio_format=clip.open_source().format, language="en"),
        )
    assert record.events[-1].event.type == "transcription.done"
    assert record.transcript  # tiny mangles the pangram; just demand text
    assert record.metrics.finalize_latency < 30
