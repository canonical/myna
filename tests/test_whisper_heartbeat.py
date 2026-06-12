"""Cold-load heartbeat behaviour (no model required).

The faster-whisper module imports the model lazily inside ``_load_model``, so
the heartbeat logic can be exercised with a stubbed load and no GPU/weights.
"""

import asyncio

from myna.core import TranscriptionProgress
from myna.testbed import whisper as whisper_mod
from myna.testbed.whisper import FasterWhisperAdapter


async def collect(adapter):
    events = []

    async def sink(event):
        events.append(event)

    await adapter._load_model_with_heartbeat(sink)
    return events


async def test_heartbeat_ticks_during_slow_load(monkeypatch):
    monkeypatch.setattr(whisper_mod, "_LOAD_HEARTBEAT_SECONDS", 0.02)
    adapter = FasterWhisperAdapter("tiny")

    async def slow_load():
        await asyncio.sleep(0.1)  # ~5 heartbeat intervals
        return object()

    monkeypatch.setattr(adapter, "_load_model", slow_load)
    events = await collect(adapter)

    assert all(isinstance(e, TranscriptionProgress) for e in events)
    assert len(events) >= 3  # immediate tick + several while loading


async def test_heartbeat_emits_once_when_warm(monkeypatch):
    adapter = FasterWhisperAdapter("tiny")

    async def instant_load():
        return object()

    monkeypatch.setattr(adapter, "_load_model", instant_load)
    events = await collect(adapter)

    assert events == [TranscriptionProgress()]  # just the immediate liveness tick
