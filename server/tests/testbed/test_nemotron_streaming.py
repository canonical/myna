"""Nemotron streaming tests (feature 008, T019/T021).

Offline half: the commit-boundary rule and the committed/unstable emission
policy (``_StreamEmitter``) are pure text logic, and the full streaming
session runs against a scripted fake ``_StreamDecoder`` — no NeMo, no GPU.

Hardware half (skips without NeMo/model): a real streaming session over an
English corpus clip passes the shared emission invariants (I1–I5) and emits
unstable hypotheses before end-of-audio (the FR-008 gap T019 closes).
"""

from __future__ import annotations

import pytest

from myna.core import (
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionFinal,
)
from myna.testbed import nemotron as nemotron_mod
from myna.testbed.nemotron import (
    NEMO_FORMAT,
    NemotronAdapter,
    _stable_commit_boundary,
    _StreamEmitter,
)

from test_emission_invariants import (
    assert_append_only_and_complete,
    assert_commit_clears_unstable,
    assert_unstable_wellformed,
)


# --- commit boundary rule ---------------------------------------------------


def test_boundary_needs_two_ticks():
    assert _stable_commit_boundary("", "hello world", 0) is None


def test_boundary_holds_back_tail_guard_words():
    # 3 stable words, guard 2 -> only the first may commit
    assert _stable_commit_boundary("the cat sat", "the cat sat on", 0) == 3
    # 2 stable words == guard -> nothing
    assert _stable_commit_boundary("hello world", "hello world again", 0) is None


def test_boundary_respects_committed_prefix():
    # committed_len=3 ("xx "); stable region after it is "the cat sat"
    assert _stable_commit_boundary("xx the cat sat", "xx the cat sat on", 3) == 6


def test_boundary_backtracks_mid_word_mutation():
    # prev tick's partial word ("satt") re-decoded ("sat down") — the common
    # prefix ends mid-word, so only whole stable words count
    assert _stable_commit_boundary("the cat satt", "the cat sat down here", 0) == 3


# --- emission policy --------------------------------------------------------


def _run_emitter(texts: list[str]) -> list:
    emitter = _StreamEmitter()
    events = []
    for text in texts:
        events.extend(emitter.update(text))
    events.extend(emitter.finish(texts[-1]))
    return events


def test_emitter_commits_stable_prefixes_and_completes():
    events = _run_emitter(
        ["the cat", "the cat sat on", "the cat sat on the mat", "the cat sat on the mat today"]
    )
    committed = [e for e in events if e.disposition == Disposition.COMMITTED]
    assert [e.text for e in committed] == ["the cat", " sat on", " the mat today"]
    assert [e.segment_index for e in committed] == [0, 1, 2]
    assert "".join(e.text for e in committed) == "the cat sat on the mat today"
    assert_unstable_wellformed(events)
    assert_commit_clears_unstable(events)


def test_emitter_unstable_is_uncommitted_remainder():
    emitter = _StreamEmitter()
    emitter.update("one two")
    events = emitter.update("one two three four five")
    unstable = [e for e in events if e.disposition == Disposition.UNSTABLE]
    assert [e.text for e in unstable] == ["one two three four five"]
    events = emitter.update("one two three four five six seven")
    committed = [e for e in events if e.disposition == Disposition.COMMITTED]
    assert [e.text for e in committed] == ["one two three"]
    unstable = [e for e in events if e.disposition == Disposition.UNSTABLE]
    assert [e.text for e in unstable] == [" four five six seven"]


def test_emitter_finish_drops_divergent_tail():
    # The final text mutated inside the committed region — the concatenation
    # stays canonical (I2) and the divergent tail is not re-committed.
    emitter = _StreamEmitter()
    emitter.update("the cat sat on")
    emitter.update("the cat sat on the mat")
    assert emitter.transcript == "the cat"
    events = emitter.finish("the dog sat on the mat")
    assert not events
    assert emitter.transcript == "the cat"


def test_emitter_empty_session():
    assert _run_emitter([""]) == []


# --- full streaming session, scripted decoder (no NeMo) ---------------------


class _FakeDecoder:
    """Scripted stand-in for _StreamDecoder: each push returns the next
    accumulated text; the final flush returns the last one."""

    SCRIPT = ["the cat", "the cat sat on", "the cat sat on the mat", "the cat sat on the mat today"]

    def __init__(self, model):
        self._i = 0

    def push(self, pcm: bytes, *, final: bool = False):
        if final:
            return self.SCRIPT[-1]
        text = self.SCRIPT[min(self._i, len(self.SCRIPT) - 1)]
        self._i += 1
        return text


async def _drive(adapter, chunks):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for chunk in chunks:
            yield chunk

    await adapter.run_session(SessionConfig(), audio(), emit)
    return events


def _pcm(seconds: float) -> PcmChunk:
    return PcmChunk(data=b"\x00\x00" * int(seconds * NEMO_FORMAT.sample_rate_hz), format=NEMO_FORMAT)


def _instant(value):
    async def _load():
        return value

    return _load


async def test_streaming_session_emits_progressively_and_completes(monkeypatch):
    adapter = NemotronAdapter(streaming=True)
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    monkeypatch.setattr(nemotron_mod, "_StreamDecoder", _FakeDecoder)

    events = await _drive(adapter, [_pcm(0.5) for _ in range(4)])

    assert any(
        isinstance(e, TranscriptionFinal) and e.disposition == Disposition.UNSTABLE
        for e in events
    ), "no unstable emission — the FR-008 gap T019 closes"
    assert_append_only_and_complete(events)
    assert_unstable_wellformed(events)
    assert_commit_clears_unstable(events)
    assert events[-1] == TranscriptionDone(text="the cat sat on the mat today")


async def test_streaming_session_empty_audio(monkeypatch):
    class _EmptyDecoder(_FakeDecoder):
        SCRIPT = [""]

    adapter = NemotronAdapter(streaming=True)
    monkeypatch.setattr(adapter, "_load_model", _instant(object()))
    monkeypatch.setattr(nemotron_mod, "_StreamDecoder", _EmptyDecoder)

    events = await _drive(adapter, [])

    assert events[-1] == TranscriptionDone(text="")
    assert not any(isinstance(e, TranscriptionFinal) for e in events)


# --- hardware integration (NeMo + model + GPU) -------------------------------

nemo = pytest.importorskip("nemo.collections.asr", reason="install with: uv sync --extra nemotron")

from pathlib import Path  # noqa: E402

from myna.core import LoopbackClient  # noqa: E402
from myna.testbed import Harness, load_manifest  # noqa: E402

CORPUS = Path(__file__).parent.parent.parent.parent / "corpus" / "real" / "manifest.json"


@pytest.fixture
def streaming_adapter():
    try:
        from nemo.collections.asr.models import ASRModel

        from myna.testbed.nemotron import DEFAULT_MODEL

        ASRModel.from_pretrained(model_name=DEFAULT_MODEL)
    except Exception as exc:  # no network / no GPU / model unavailable
        pytest.skip(f"Nemotron model unavailable: {exc}")
    return NemotronAdapter(streaming=True)


@pytest.mark.skipif(not CORPUS.exists(), reason="real corpus not present")
async def test_nemotron_streaming_session_invariants(streaming_adapter):
    clips = {clip.id: clip for clip in load_manifest(CORPUS)}
    clip = clips["librispeech-2277-149896-0027"]  # 3.6 s English speech
    source = clip.open_source()
    record = await Harness().run(
        client=LoopbackClient(streaming_adapter),
        candidate=streaming_adapter.candidate,
        source=source,
        config=SessionConfig(audio_format=source.format, language="en"),
    )
    events = [te.event for te in record.events]

    assert_append_only_and_complete(events)
    assert_unstable_wellformed(events)
    assert_commit_clears_unstable(events)
    assert any(
        isinstance(e, TranscriptionFinal) and e.disposition == Disposition.UNSTABLE
        for e in events
    ), "native streaming must emit unstable hypotheses before end-of-audio"
    assert record.transcript
