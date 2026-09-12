"""Whisper adapter streaming/batch session tests (feature 008, T010).

Model-backed (whisper-tiny on CPU) over the synthetic fixtures: batch mode is
degenerate streaming (I7 — one committed segment, done == final), and a
streaming session passes the emission invariants (I1–I5). Synthetic espeak
audio keeps accuracy assertions out of scope — this checks event flow only.
"""

from __future__ import annotations

from pathlib import Path

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from test_emission_invariants import (
    assert_append_only_and_complete,
    assert_batch_degenerate,
    assert_unstable_wellformed,
)

from myna.core import LoopbackClient, SessionConfig
from myna.testbed import Harness
from myna.testbed.corpus import load_manifest
from myna.testbed.whisper import FasterWhisperAdapter

MANIFEST = Path(__file__).parent.parent / "fixtures" / "manifest.json"

pytestmark = pytest.mark.skipif(
    not MANIFEST.exists(), reason="run `python dev/generate_fixtures.py` from repo root first"
)


def _tiny(**kwargs) -> FasterWhisperAdapter:
    try:
        from faster_whisper import WhisperModel  # noqa: F401

        WhisperModel("tiny", device="cpu")
    except Exception as exc:  # model unavailable offline
        pytest.skip(f"whisper-tiny model unavailable: {exc}")
    return FasterWhisperAdapter("tiny", **kwargs)


async def test_batch_mode_is_degenerate_streaming():
    adapter = _tiny()  # streaming off
    clip = load_manifest(MANIFEST)[0]
    source = clip.open_source()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=source,
        config=SessionConfig(audio_format=source.format, language="en"),
    )
    events = [te.event for te in record.events]
    assert_batch_degenerate(events)


async def test_streaming_mode_emits_progressively_and_completes():
    adapter = _tiny(streaming=True, strategy="local-agreement")
    clip = load_manifest(MANIFEST)[0]
    source = clip.open_source()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=source,
        config=SessionConfig(audio_format=source.format, language="en"),
    )
    events = [te.event for te in record.events]
    assert_append_only_and_complete(events)
    assert_unstable_wellformed(events)
    assert record.metrics.commit_stability
