"""IE115 wire-dialect parity + codec tests (T45).

Two levels:

- **Codec unit tests** — the pure ``myna.core.wire_ie115`` translation both ways
  (session config, each event type, the base64 audio append).
- **End-to-end parity** — the fake adapter driven by the harness over the IE115
  dialect (``WsUnixIe115Client`` ↔ shape-sniffing ``serve_unix``), asserting the
  session contract survives the OpenAI-shaped wire, plus the *deliberate*
  divergences (STATUS liveness, lossy error mapping) that make it IE115 and not
  the internal wire.
"""

from __future__ import annotations

import contextlib

from myna.core import (
    AudioFormat,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
    WsUnixIe115Client,
    serve_unix,
)
from myna.core import wire_ie115 as w
from myna.testbed import FakeAdapter, Harness, ScriptStep, SilenceSource

TERMINAL = ("transcription.done", "transcription.error")


# --- codec unit tests -----------------------------------------------------------


def test_session_config_round_trips_through_ie115():
    config = SessionConfig(
        audio_format=AudioFormat(sample_rate_hz=16_000), language="en", prompt="proper nouns"
    )
    session = w.session_config_to_ie115(config, model="whisper-base")
    # nested OpenAI shape
    assert session["type"] == "realtime"
    inp = session["audio"]["input"]
    assert inp["format"] == {"type": "audio/pcm", "rate": 16_000}
    assert inp["transcription"] == {
        "model": "whisper-base",
        "language": "en",
        "prompt": "proper nouns",
    }
    # and back
    back = w.session_config_from_ie115(session)
    assert back.audio_format.sample_rate_hz == 16_000
    assert back.language == "en"
    assert back.prompt == "proper nouns"


def test_session_config_from_ie115_defaults_when_sparse():
    back = w.session_config_from_ie115({"type": "realtime"})
    assert back.audio_format == AudioFormat()  # mono 16-bit 16k default
    assert back.language is None


def test_append_round_trips_base64():
    fmt = AudioFormat()
    chunk = PcmChunk(data=b"\x01\x02\x03\x04", format=fmt)
    frame = w.pcm_to_append(chunk)
    assert frame["type"] == w.INPUT_AUDIO_APPEND
    assert isinstance(frame["audio"], str)  # base64 text, not raw bytes
    back = w.append_to_pcm(frame, fmt)
    assert back.data == b"\x01\x02\x03\x04"


def test_encoder_progress_phases_map_to_status_states():
    enc = w.Ie115Encoder()
    assert enc.encode(TranscriptionProgress(phase="preparing"))["state"] == "loading"
    assert enc.encode(TranscriptionProgress(phase="ready"))["state"] == "ready"
    transcribing = enc.encode(TranscriptionProgress(phase="transcribing", snippet="hi"))
    assert transcribing == {"type": w.STATUS_EVENT, "state": "transcribing", "snippet": "hi"}


def test_encoder_progress_warning_is_additive_and_round_trips():
    # T10: memory-pressure notice rides the same additive STATUS frame.
    enc = w.Ie115Encoder()
    frame = enc.encode(TranscriptionProgress(phase="transcribing", warning="low on memory"))
    assert frame == {"type": w.STATUS_EVENT, "state": "transcribing", "warning": "low on memory"}
    dec = w.Ie115Decoder()
    [event] = dec.decode(frame)
    assert event == TranscriptionProgress(phase="transcribing", warning="low on memory")


def test_encoder_finals_become_deltas_sharing_the_utterance_item():
    enc = w.Ie115Encoder()
    d1 = enc.encode(TranscriptionFinal(text="one"))
    d2 = enc.encode(TranscriptionFinal(text="two"))
    assert d1["type"] == w.TRANSCRIPTION_DELTA
    assert d1["delta"] == "one" and d1["content_index"] == 0
    assert d1["item_id"] and d1["item_id"] == d2["item_id"]  # one item per utterance


def test_encoder_done_becomes_completed_and_retires_the_item():
    enc = w.Ie115Encoder()
    delta = enc.encode(TranscriptionFinal(text="one"))
    done = enc.encode(TranscriptionDone(text="one two"))
    assert done["type"] == w.TRANSCRIPTION_COMPLETED
    assert done["transcript"] == "one two"
    assert done["item_id"] == delta["item_id"]  # completed closes the same item
    # the next utterance on the same (persistent) connection is a new item
    next_done = enc.encode(TranscriptionDone(text="three"))
    assert next_done["item_id"] != done["item_id"]


def test_encoder_error_maps_lossily_and_decoder_recovers_ie115_code():
    enc = w.Ie115Encoder()
    frame = enc.encode(TranscriptionError(code="adapter_crash", message="boom"))
    assert frame == {
        "type": w.ERROR,
        "error": {"type": "server_error", "code": "server_error", "message": "boom"},
    }
    # the internal `adapter_crash` is *not* recoverable from the wire — the
    # mapping is lossy on purpose (T31 evidence).
    dec = w.Ie115Decoder()
    (event,) = dec.decode(frame)
    assert isinstance(event, TranscriptionError)
    assert event.code == "server_error"  # not "adapter_crash"


def test_decoder_delta_is_committed_final_and_completed_is_done():
    dec = w.Ie115Decoder()
    (final,) = dec.decode(
        {"type": w.TRANSCRIPTION_DELTA, "item_id": "i1", "content_index": 0, "delta": "one"}
    )
    assert isinstance(final, TranscriptionFinal) and final.text == "one"
    (done,) = dec.decode(
        {
            "type": w.TRANSCRIPTION_COMPLETED,
            "item_id": "i1",
            "content_index": 0,
            "transcript": "one two",
        }
    )
    assert isinstance(done, TranscriptionDone) and done.text == "one two"
    assert dec.on_close() == []  # terminal already delivered; close is just close


def test_decoder_close_before_completed_is_an_error_not_a_done():
    # A dead server must never read as a successful (possibly truncated) utterance.
    dec = w.Ie115Decoder()
    dec.decode({"type": w.TRANSCRIPTION_DELTA, "item_id": "i1", "content_index": 0, "delta": "one"})
    (event,) = dec.on_close()
    assert isinstance(event, TranscriptionError)
    assert event.code == "connection_closed"
    assert dec.on_close() == []  # idempotent


def test_decoder_no_double_terminal_after_error():
    dec = w.Ie115Decoder()
    dec.decode(
        {"type": w.ERROR, "error": {"type": "server_error", "code": "server_error", "message": "x"}}
    )
    assert dec.on_close() == []  # error already terminal


def test_decoder_ignores_control_frames():
    dec = w.Ie115Decoder()
    assert dec.decode({"type": w.SESSION_CREATED, "session": {}}) == []
    assert dec.decode({"type": w.SESSION_UPDATED, "session": {}}) == []


# --- end-to-end parity over the wire --------------------------------------------


@contextlib.asynccontextmanager
async def ie115_transport(service, tmp_path, *, base64_audio=False):
    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(service, socket_path):
        yield WsUnixIe115Client(socket_path, base64_audio=base64_audio)


async def _run(tmp_path, *, adapter=None, base64_audio=False, duration=0.2):
    adapter = adapter or FakeAdapter()
    async with ie115_transport(adapter, tmp_path, base64_audio=base64_audio) as client:
        return await Harness().run(
            client=client,
            candidate=adapter.candidate,
            source=SilenceSource(duration_seconds=duration),
        )


async def test_ie115_preserves_the_session_contract(tmp_path):
    record = await _run(tmp_path)
    kinds = [te.event.type for te in record.events]
    # exactly one terminal, and it is last
    assert sum(k in TERMINAL for k in kinds) == 1
    assert kinds[-1] in TERMINAL
    # done carries the full transcript, assembled from the completed segments
    assert record.transcript == "The quick brown fox jumps over the lazy dog."
    finals = [te.event.text for te in record.events if te.event.type == "transcription.final"]
    assert finals == ["The quick brown fox", "jumps over the lazy dog."]
    # monotonic timestamps
    times = [te.t for te in record.events]
    assert times == sorted(times)


async def test_ie115_surfaces_status_liveness(tmp_path):
    """The residency liveness (preparing/ready) rides the additive STATUS event
    and decodes back to progress phases — the gate the client needs (T42)."""
    record = await _run(tmp_path)
    phases = [te.event.phase for te in record.events if te.event.type == "transcription.progress"]
    assert "preparing" in phases
    assert "ready" in phases
    assert phases.index("preparing") < phases.index("ready")


async def test_ie115_base64_audio_path(tmp_path):
    """The OpenAI-parity append path (base64-in-JSON) transcribes identically to
    binary frames — the frame-type hatch (note §5)."""
    record = await _run(tmp_path, base64_audio=True)
    assert record.transcript == "The quick brown fox jumps over the lazy dog."


async def test_ie115_error_mapping_is_lossy_but_terminal(tmp_path):
    """An adapter crash still terminates cleanly over IE115, but the specific
    internal code collapses onto IE115's `server_error` — recorded T31 evidence."""

    class _CrashingAdapter:
        candidate = FakeAdapter().candidate

        def capabilities(self):
            return FakeAdapter().capabilities()

        async def run_session(self, config, audio, emit: EventSink) -> None:
            async for _ in audio:
                pass
            raise RuntimeError("boom")

    record = await _run(tmp_path, adapter=_CrashingAdapter())
    kinds = [te.event.type for te in record.events]
    assert kinds[-1] == "transcription.error"
    assert record.events[-1].event.code == "server_error"  # lossy: not "adapter_crash"


async def test_ie115_custom_single_final(tmp_path):
    adapter = FakeAdapter(
        script=(ScriptStep(0.0, TranscriptionFinal(text="hi")),), done_after_audio_ends=False
    )
    record = await _run(tmp_path, adapter=adapter)
    assert record.transcript == "hi"


async def test_ie115_stock_client_can_wait_for_the_greeting(tmp_path):
    """A stock OpenAI Realtime client sends nothing until it has seen
    ``session.created`` — the server's eager greeting keeps that client from
    deadlocking against the shape-sniff, and the session then runs normally."""
    import json

    from websockets.asyncio.client import unix_connect

    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        async with unix_connect(str(socket_path)) as ws:
            greeting = json.loads(await ws.recv())  # before sending anything
            assert greeting["type"] == w.SESSION_CREATED
            assert "session" in greeting  # server defaults
            await ws.send(json.dumps({"type": w.SESSION_UPDATE, "session": {"type": "realtime"}}))
            updated = json.loads(await ws.recv())
            assert updated["type"] == w.SESSION_UPDATED
            await ws.send(b"\x00" * 3200)
            await ws.send(json.dumps({"type": w.INPUT_AUDIO_COMMIT}))
            while True:
                frame = json.loads(await ws.recv())
                assert frame["type"] != w.ERROR
                if frame["type"] == w.TRANSCRIPTION_COMPLETED:
                    break


async def test_ie115_rejects_a_model_this_server_does_not_serve(tmp_path):
    """One model per process: a ``session.update`` naming a model this server
    does not serve is rejected with an error, never silently answered by a
    different model (a compat client asking for X must not get Y)."""
    import json

    from websockets.asyncio.client import unix_connect

    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(FakeAdapter(), socket_path):  # serves only "fake"
        async with unix_connect(str(socket_path)) as ws:
            json.loads(await ws.recv())  # the greeting
            await ws.send(
                json.dumps(
                    {
                        "type": w.SESSION_UPDATE,
                        "session": {
                            "type": "realtime",
                            "audio": {"input": {"transcription": {"model": "whisper-large"}}},
                        },
                    }
                )
            )
            reply = json.loads(await ws.recv())
            assert reply["type"] == w.ERROR
            assert reply["error"]["type"] == "invalid_request_error"
            assert reply["error"]["code"] == "invalid_parameter"
            assert "whisper-large" in reply["error"]["message"]


async def test_ie115_serves_a_correctly_named_model(tmp_path):
    """Naming the model the server actually serves is acknowledged, and the
    ``session.updated`` echo carries it back."""
    import json

    from websockets.asyncio.client import unix_connect

    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        async with unix_connect(str(socket_path)) as ws:
            json.loads(await ws.recv())  # the greeting
            await ws.send(
                json.dumps(
                    {
                        "type": w.SESSION_UPDATE,
                        "session": {
                            "type": "realtime",
                            "audio": {"input": {"transcription": {"model": "fake"}}},
                        },
                    }
                )
            )
            updated = json.loads(await ws.recv())
            assert updated["type"] == w.SESSION_UPDATED
            assert updated["session"]["audio"]["input"]["transcription"]["model"] == "fake"


async def test_ie115_connection_persists_across_commits(tmp_path):
    """The OpenAI multi-commit shape (decided 2026-07-06): one connection carries
    several commit cycles, each answered by its own `completed`; the server does
    not close after a terminal — the client closes when it is finished."""
    import json

    from websockets.asyncio.client import unix_connect

    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        async with unix_connect(str(socket_path)) as ws:
            await ws.send(json.dumps({"type": w.SESSION_UPDATE, "session": {"type": "realtime"}}))
            transcripts, items = [], []
            for _ in range(2):
                await ws.send(b"\x00" * 3200)  # ~100 ms of PCM
                await ws.send(json.dumps({"type": w.INPUT_AUDIO_COMMIT}))
                deltas = []
                while True:
                    frame = json.loads(await ws.recv())
                    assert frame["type"] != w.ERROR
                    if frame["type"] == w.TRANSCRIPTION_DELTA:
                        deltas.append(frame)
                    if frame["type"] == w.TRANSCRIPTION_COMPLETED:
                        transcripts.append(frame["transcript"])
                        items.append(frame["item_id"])
                        # every delta of the utterance shares the completed's item
                        assert all(d["item_id"] == frame["item_id"] for d in deltas)
                        break
    assert transcripts == ["The quick brown fox jumps over the lazy dog."] * 2
    assert items[0] != items[1]  # one conversation item per utterance
