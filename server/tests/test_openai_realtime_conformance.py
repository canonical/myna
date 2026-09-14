"""OpenAI Realtime Transcription API conformance.

The README promises the IE115 dialect is "a compatible subset of the OpenAI
Realtime Transcription API, with additions". This suite is what makes that
sentence a fact rather than an intention: every frame the server emits on an
IE115 connection is validated against the ``openai`` SDK's pydantic models,
which are generated from OpenAI's published OpenAPI spec. The SDK version
pinned in ``pyproject.toml`` is therefore the spec revision we claim.

The subset: stock ``session.update`` / ``input_audio_buffer.append`` /
``input_audio_buffer.commit`` in; ``session.created`` / ``session.updated`` /
``input_audio_buffer.committed`` / ``conversation.item.input_audio_transcription.
delta`` / ``...completed`` / ``error`` out, each a valid instance of its event.

The additions (the ``status`` event; the ``disposition``, ``segment_index``,
``segments``, ``protocol_version`` and ``streaming`` fields) ride on top: an
addition must be something a stock client survives, and this suite proves the
SDK's own parser does.

Audio: OpenAI's ``audio/pcm`` is 24 kHz and nothing else, and that is what
the greeting advertises and a stock client sends. The adapters take 16 kHz;
the dialect edge resamples, so the adapter never learns the wire rate. Our
own clients state their real capture rate instead (an addition: any rate,
resampled at the edge), which is the one place their frames leave the schema.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import copy
import json
from typing import Any, cast, get_args

import numpy as np
import openai.types.realtime as rt
import pytest
from openai.resources.realtime.realtime import AsyncRealtimeConnection
from websockets.asyncio.client import unix_connect
from websockets.exceptions import ConnectionClosed

from myna.core import AudioFormat, PcmChunk, SessionConfig, TranscriptionFinal, serve_unix
from myna.core import wire_ie115 as w
from myna.core.events import Segment, TranscriptionDone
from myna.testbed import FakeAdapter

# --- the oracle ------------------------------------------------------------------


def _by_type(union: Any) -> dict[str, type[Any]]:
    """``{"session.created": SessionCreatedEvent, ...}`` from an SDK event union,
    keyed by the ``type`` literal each model declares."""
    out: dict[str, type[Any]] = {}
    for member in get_args(get_args(union)[0]):
        for literal in get_args(member.model_fields["type"].annotation):
            out[literal] = member
    return out


SERVER_EVENTS = _by_type(rt.RealtimeServerEvent)
CLIENT_EVENTS = _by_type(rt.RealtimeClientEvent)

# Event types we add on top of the OpenAI vocabulary. Anything else the server
# sends must be an OpenAI event, field for field.
ADDITIVE_EVENTS = frozenset({w.STATUS_EVENT})

# The SDK's own frame parser: what a stock ``openai`` client runs on every
# message. It never needs the socket, only the event vocabulary.
_stock_parser = AsyncRealtimeConnection(cast(Any, None))


def as_openai_server_event(frame: dict[str, Any]) -> Any:
    """Validate ``frame`` as the OpenAI server event its ``type`` names.
    Raises on an unknown type or a missing/mistyped required field."""
    assert frame["type"] in SERVER_EVENTS, f"not an OpenAI server event: {frame['type']!r}"
    return SERVER_EVENTS[frame["type"]].model_validate(frame)


def as_openai_client_event(frame: dict[str, Any]) -> Any:
    assert frame["type"] in CLIENT_EVENTS, f"not an OpenAI client event: {frame['type']!r}"
    return CLIENT_EVENTS[frame["type"]].model_validate(frame)


def without_our_rate(frame: dict[str, Any]) -> dict[str, Any]:
    """Our own clients state their capture rate, which OpenAI's closed
    ``audio/pcm`` literal cannot hold: drop it to hold the rest to the schema."""
    frame = copy.deepcopy(frame)
    with contextlib.suppress(KeyError, TypeError):
        del frame["session"]["audio"]["input"]["format"]
    return frame


# --- a stock client session ------------------------------------------------------

_CHUNK = b"\x00" * 4800  # 100 ms of 24 kHz mono S16LE, OpenAI's one PCM rate
_CHUNKS = 3


def stock_session_update(model: str, **transcription: Any) -> dict[str, Any]:
    """A ``session.update`` exactly as the SDK would build it for a
    transcription session, 24 kHz PCM stated the only way the SDK allows."""
    event = rt.SessionUpdateEvent(
        type="session.update",
        session=rt.RealtimeTranscriptionSessionCreateRequest(
            type="transcription",
            audio=rt.RealtimeTranscriptionSessionAudio(
                input=rt.RealtimeTranscriptionSessionAudioInput(
                    format=rt.realtime_audio_formats.AudioPCM(type="audio/pcm", rate=24_000),
                    transcription=rt.AudioTranscription(model=model, **transcription),
                )
            ),
        ),
    )
    return event.model_dump(mode="json", exclude_none=True)


async def run_stock_utterance(
    socket_path: Any, chunks: list[bytes] | None = None
) -> list[dict[str, Any]]:
    """One utterance the way a stock client drives it: wait for the greeting,
    configure, append base64 audio, commit, read to ``completed``. Returns
    every server frame seen, in order."""
    frames: list[dict[str, Any]] = []
    async with unix_connect(str(socket_path), ping_interval=None) as ws:
        frames.append(json.loads(await ws.recv()))  # server speaks first
        await ws.send(json.dumps(stock_session_update("fake", language="en")))
        frames.append(json.loads(await ws.recv()))
        for chunk in chunks if chunks is not None else [_CHUNK] * _CHUNKS:
            append = rt.InputAudioBufferAppendEvent(
                type="input_audio_buffer.append",
                audio=base64.b64encode(chunk).decode("ascii"),
            )
            await ws.send(json.dumps(append.model_dump(mode="json", exclude_none=True)))
        commit = rt.InputAudioBufferCommitEvent(type="input_audio_buffer.commit")
        await ws.send(json.dumps(commit.model_dump(mode="json", exclude_none=True)))
        while True:
            frame = json.loads(await ws.recv())
            frames.append(frame)
            if frame["type"] in (w.TRANSCRIPTION_COMPLETED, w.ERROR):
                return frames


@pytest.fixture
async def stock_frames(tmp_path):
    socket_path = tmp_path / "myna.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        return await run_stock_utterance(socket_path)


def _kinds(frames: list[dict[str, Any]]) -> list[str]:
    return [f["type"] for f in frames]


# --- server -> client ------------------------------------------------------------


async def test_every_frame_is_an_openai_event_or_a_declared_addition(stock_frames):
    for frame in stock_frames:
        if frame["type"] in ADDITIVE_EVENTS:
            continue
        as_openai_server_event(frame)  # raises with the offending field
    kinds = _kinds(stock_frames)
    assert kinds[:2] == [w.SESSION_CREATED, w.SESSION_UPDATED]
    assert kinds[-1] == w.TRANSCRIPTION_COMPLETED
    assert w.TRANSCRIPTION_DELTA in kinds


async def test_every_event_id_is_present_and_unique(stock_frames):
    """OpenAI stamps every server event with an ``event_id``; clients key
    logs, retries and error attribution on it."""
    ids = [f.get("event_id") for f in stock_frames if f["type"] not in ADDITIVE_EVENTS]
    assert all(isinstance(i, str) and i for i in ids)
    assert len(set(ids)) == len(ids)


async def test_the_commit_is_acknowledged_with_the_utterance_item(stock_frames):
    """A stock client learns the conversation item from
    ``input_audio_buffer.committed`` and joins the deltas and the completed
    transcript on it."""
    by_kind: dict[str, list[dict[str, Any]]] = {}
    for frame in stock_frames:
        by_kind.setdefault(frame["type"], []).append(frame)
    [committed] = by_kind[w.INPUT_AUDIO_COMMITTED]
    [completed] = by_kind[w.TRANSCRIPTION_COMPLETED]
    assert committed["item_id"] == completed["item_id"]
    assert all(d["item_id"] == committed["item_id"] for d in by_kind[w.TRANSCRIPTION_DELTA])


async def test_completed_reports_the_audio_duration_as_usage(stock_frames):
    """``usage`` is required on ``completed``. We have no tokens to bill, so
    it is the duration form: the seconds of PCM the utterance consumed."""
    completed = as_openai_server_event(stock_frames[-1])
    assert completed.usage.type == "duration"
    assert completed.usage.seconds == pytest.approx(_CHUNKS * 0.1)


async def test_a_rejected_model_is_an_openai_error_event(tmp_path):
    socket_path = tmp_path / "myna.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        async with unix_connect(str(socket_path), ping_interval=None) as ws:
            as_openai_server_event(json.loads(await ws.recv()))
            await ws.send(json.dumps(stock_session_update("gpt-4o-transcribe")))
            error = as_openai_server_event(json.loads(await ws.recv()))
    assert error.type == "error"
    assert error.error.type == "invalid_request_error"
    assert "gpt-4o-transcribe" in error.error.message


async def test_an_adapter_that_ends_without_a_terminal_yields_an_openai_error(tmp_path):
    """The one-terminal contract is the server's to keep on this wire too: a
    stock client blocked on ``completed`` gets a valid ``error``, not a hang."""

    class NoTerminal:
        def capabilities(self):
            return FakeAdapter().capabilities()

        async def run_session(self, config, audio, emit):
            async for _ in audio:
                pass  # consumed everything, said nothing

    socket_path = tmp_path / "myna.sock"
    async with serve_unix(NoTerminal(), socket_path):
        frames = await run_stock_utterance(socket_path)
    error = as_openai_server_event(frames[-1])
    assert error.type == "error"
    assert error.error.type == "server_error"


async def test_a_malformed_append_is_an_openai_error_not_a_hang(tmp_path):
    """Audio that is not base64 cannot be fed to the adapter. The stock client
    gets a valid ``error`` and the connection ends; it must never sit waiting
    on a ``completed`` that cannot come."""
    socket_path = tmp_path / "myna.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        async with unix_connect(str(socket_path), ping_interval=None) as ws:
            await ws.recv()
            await ws.send(json.dumps(stock_session_update("fake")))
            await ws.recv()
            await ws.send(json.dumps({"type": "input_audio_buffer.append", "audio": 12345}))
            await ws.send(json.dumps({"type": "input_audio_buffer.commit"}))
            while True:
                frame = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
                if frame["type"] == w.ERROR:
                    break
            error = as_openai_server_event(frame)
            assert error.error.type == "invalid_request_error"
            with pytest.raises(ConnectionClosed):
                while True:  # anything else must lead to the close
                    await asyncio.wait_for(ws.recv(), timeout=5)


async def test_a_client_that_vanishes_mid_utterance_does_not_hang_the_adapter(tmp_path):
    """A stock client that drops the socket before committing ends the
    utterance: the adapter's audio runs out and its session returns, so the
    server neither hangs nor keeps the tail for a peer that is gone."""
    finished = asyncio.Event()

    class Ending:
        def capabilities(self):
            return FakeAdapter().capabilities()

        async def run_session(self, config, audio, emit):
            async for _ in audio:
                pass
            finished.set()
            await emit(TranscriptionDone(text=""))

    socket_path = tmp_path / "myna.sock"
    async with serve_unix(Ending(), socket_path):
        ws = await unix_connect(str(socket_path), ping_interval=None)
        await ws.recv()
        await ws.send(json.dumps(stock_session_update("fake")))
        await ws.recv()
        await ws.send(
            json.dumps(
                {
                    "type": "input_audio_buffer.append",
                    "audio": base64.b64encode(_CHUNK).decode("ascii"),
                }
            )
        )
        ws.transport.abort()  # no close handshake: the process died
        await asyncio.wait_for(finished.wait(), timeout=5)


def test_additions_on_a_delta_do_not_break_the_event():
    """Our extra fields (``disposition``, ``segment_index``, ``segments``) must
    leave the delta a valid OpenAI delta: additive means additive."""
    encoder = w.Ie115Encoder()
    frame = encoder.encode(
        TranscriptionFinal(
            text="hi there",
            segment_index=0,
            segments=(Segment(start=0.4, end=1.1, text="hi there", score=-0.3),),
        )
    )
    delta = as_openai_server_event(frame)
    assert delta.delta == "hi there"


def test_additive_events_are_outside_the_openai_vocabulary_and_parse_leniently():
    """An event we add must not collide with a real OpenAI event now or later
    (this trips if OpenAI ever defines one with the same name), and a stock
    client's parser must not choke on it."""
    for kind in ADDITIVE_EVENTS:
        assert kind not in SERVER_EVENTS
    status = json.dumps({"type": w.STATUS_EVENT, "state": "ready", "event_id": "event_x"})
    _stock_parser.parse_event(status)  # must not raise


# --- client -> server ------------------------------------------------------------


def test_our_own_client_frames_are_openai_client_events():
    """The Python (and by mirror, the Rust) IE115 client must be a stock
    client to any OpenAI-shaped peer, not only to our server."""
    config = SessionConfig(audio_format=AudioFormat(), language="en", prompt="nouns")
    update = {"type": w.SESSION_UPDATE, "session": w.session_config_to_ie115(config)}
    session = as_openai_client_event(without_our_rate(update)).session
    assert session.type == "transcription"
    assert session.audio.input.transcription.language == "en"
    as_openai_client_event(w.pcm_to_append(PcmChunk(data=_CHUNK, format=AudioFormat())))
    as_openai_client_event({"type": w.INPUT_AUDIO_COMMIT})


# --- audio rate ------------------------------------------------------------------


def test_the_greeting_advertises_openais_one_pcm_rate():
    """OpenAI's ``audio/pcm`` admits exactly one rate. The greeting's default
    is that rate, not the adapter's: a stock client that reads the greeting
    and one that assumes 24 kHz must agree. This trips if OpenAI ever admits
    another rate, which is when the default needs a second look."""
    pcm = cast(Any, rt.realtime_audio_formats.AudioPCM)
    rate = pcm.model_fields["rate"].annotation  # Literal[24000] | None today
    admitted = {value for member in get_args(rate) for value in get_args(member)}
    assert admitted == {w.IE115_PCM_RATE}
    greeting = w.session_config_to_ie115(w.ie115_session_defaults())
    assert greeting["audio"]["input"]["format"] == {"type": "audio/pcm", "rate": 24_000}


class _RecordingAdapter:
    """Accepts only 16 kHz, like every real adapter, and keeps what it heard."""

    def __init__(self) -> None:
        self.formats: list[AudioFormat] = []
        self.pcm = b""

    def capabilities(self):
        return FakeAdapter().capabilities()

    async def run_session(self, config, audio, emit):
        self.formats.append(config.audio_format)
        async for chunk in audio:
            assert chunk.format == AudioFormat(sample_rate_hz=16_000)
            self.pcm += chunk.data
        await emit(TranscriptionDone(text="heard"))


async def test_a_stock_24k_client_reaches_the_adapter_at_16k(tmp_path):
    """The edge resamples: a 24 kHz tone from a stock client arrives at the
    adapter as the same tone at 16 kHz, in 100 ms chunks, with the adapter
    never told a different rate existed."""
    t = np.arange(24_000) / 24_000  # one second
    tone = (np.sin(2 * np.pi * 440 * t) * 0.5 * 32767).astype("<i2").tobytes()
    chunks = [tone[i : i + 4800] for i in range(0, len(tone), 4800)]
    adapter = _RecordingAdapter()
    socket_path = tmp_path / "myna.sock"
    async with serve_unix(adapter, socket_path):
        frames = await run_stock_utterance(socket_path, chunks)

    # two runs: the utterance, then the eager (empty) one the next commit
    # would have fed; both at the adapter's rate, neither told of 24 kHz
    assert set(adapter.formats) == {AudioFormat(sample_rate_hz=16_000)}
    heard = np.frombuffer(adapter.pcm, dtype="<i2").astype(np.float64) / 32767
    assert len(heard) == 16_000  # one second at the adapter's rate
    expected = np.sin(2 * np.pi * 440 * np.arange(16_000) / 16_000) * 0.5
    assert np.abs(heard[200:-200] - expected[200:-200]).max() < 0.01
    # the echo tells the client what it asked for, and usage is wall time
    updated = as_openai_server_event(frames[1])
    assert updated.session.audio.input.format.rate == 24_000
    completed = as_openai_server_event(frames[-1])
    assert completed.usage.seconds == pytest.approx(1.0)
