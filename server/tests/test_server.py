"""Integration test for the standalone UbuSTT server (T14a).

Spawns ``python -m myna.server`` as a real subprocess and transcribes a
fixture clip through its Unix socket — the same path the whisper snap's
daemon takes. Skips without the whisper extra or a cached model.
"""

import asyncio
import sys
from pathlib import Path

import pytest

pytest.importorskip("faster_whisper", reason="install with: uv sync --extra whisper")

from myna.core import SessionConfig, WsUnixClient
from myna.testbed import Harness, load_manifest
from myna.testbed.whisper import FasterWhisperAdapter

MANIFEST = Path(__file__).parent.parent / "fixtures" / "manifest.json"

pytestmark = pytest.mark.skipif(
    not MANIFEST.exists(), reason="run `python dev/generate_fixtures.py` from repo root first"
)


@pytest.fixture
def tiny_model_cached():
    try:
        from faster_whisper import WhisperModel

        WhisperModel("tiny", device="cpu")
    except Exception as exc:
        pytest.skip(f"whisper-tiny model unavailable: {exc}")


async def test_server_subprocess_end_to_end(tiny_model_cached, tmp_path):
    socket_path = tmp_path / "ubustt.sock"
    proc = await asyncio.create_subprocess_exec(
        sys.executable,
        "-m",
        "myna.server",
        "--socket",
        str(socket_path),
        "--model",
        "tiny",
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        for _ in range(100):  # the socket appears as soon as the server binds
            if socket_path.exists():
                break
            await asyncio.sleep(0.1)
        else:
            stderr = await proc.stderr.read()
            pytest.fail(f"server did not bind socket; stderr:\n{stderr.decode()}")

        clips = {clip.id: clip for clip in load_manifest(MANIFEST)}
        clip = clips["quiet-weather"]
        record = await Harness().run(
            client=WsUnixClient(socket_path),
            candidate=FasterWhisperAdapter("tiny").candidate,
            source=clip.open_source(),
            config=SessionConfig(audio_format=clip.open_source().format, language="en"),
        )
        assert record.events[-1].event.type == "transcription.done"
        assert "afternoon" in record.transcript.lower()
    finally:
        proc.terminate()
        await proc.wait()
    assert not socket_path.exists()  # cleaned up on SIGTERM
