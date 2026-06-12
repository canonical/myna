"""Integration test for the Nemotron adapter (T09).

Needs the ``nemotron`` extra (NeMo + torch) and the model (downloaded on
first run). Skips cleanly when either is unavailable, so the default offline
suite stays green. English-only model, so it uses an English fixture.

Like the whisper integration test, this verifies the contract and plumbing,
not WER — that's the matrix's job.
"""

from pathlib import Path

import pytest

pytest.importorskip("nemo.collections.asr", reason="install with: uv sync --extra nemotron")

from myna.core import LoopbackClient, SessionConfig
from myna.testbed import Harness, load_manifest
from myna.testbed.nemotron import NemotronAdapter

MANIFEST = Path(__file__).parent.parent / "fixtures" / "manifest.json"

pytestmark = pytest.mark.skipif(
    not MANIFEST.exists(), reason="run `uv run python dev/generate_fixtures.py` first"
)


@pytest.fixture
def adapter():
    try:
        from nemo.collections.asr.models import ASRModel

        from myna.testbed.nemotron import DEFAULT_MODEL

        ASRModel.from_pretrained(model_name=DEFAULT_MODEL)
    except Exception as exc:  # no network / no GPU / model unavailable
        pytest.skip(f"Nemotron model unavailable: {exc}")
    return NemotronAdapter()


async def test_nemotron_transcribes_english_fixture(adapter):
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
    assert "transcription.error" not in kinds
    assert record.transcript  # native punctuation/caps; just demand text
