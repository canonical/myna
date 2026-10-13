"""Offline contract tests for the embedded Lemonade AMD-NPU adapter."""

from __future__ import annotations

import json
import urllib.error
import urllib.request

import numpy as np
import pytest

import myna.testbed.lemond as lemond
from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    AudioFormat,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.lemond import LEMOND_FORMAT, LemondAdapter
from myna.testbed.streaming import batch


async def _drive(adapter, config=None, chunks=()):
    config = config or SessionConfig()
    events = []

    async def audio():
        for chunk in chunks:
            yield chunk

    async def emit(event):
        events.append(event)

    await adapter.run_session(config, audio(), emit)
    return events


async def _single_batch(_audio, _emit, decode, on_commit):
    """Keep these adapter tests independent of the shared streaming loop."""
    hypothesis = decode(np.zeros(16_000, dtype=np.float32), 0.0)
    await on_commit("".join(word.text for word in hypothesis.words), hypothesis.words)


def test_capabilities_describe_the_embedded_lemond_hrx_npu_runtime():
    adapter = LemondAdapter(api_key="secret")
    caps = adapter.capabilities()

    assert adapter.candidate.id == "whisper-v3-turbo-FLM/lemond-flm-hrx-npu/commit-on-finalize"
    assert caps.models == ("whisper-v3-turbo-FLM",)
    assert caps.input_formats == (LEMOND_FORMAT,)
    assert caps.streaming is False
    assert caps.punctuation is True
    assert caps.translation is False
    assert caps.runtime == {
        "inference_engine": "Lemonade Server",
        "backend": "FastFlowLM",
        "npu_runtime": "HRX",
        "device": "npu",
    }


async def test_rejects_audio_before_contacting_lemond(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def unexpected_ready(_emit):
        raise AssertionError("off-format audio must not contact lemond")

    monkeypatch.setattr(adapter, "_wait_ready_with_heartbeat", unexpected_ready)
    events = await _drive(
        adapter,
        SessionConfig(audio_format=AudioFormat(sample_rate_hz=48_000)),
    )

    assert len(events) == 1
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


async def test_rejects_translation_before_contacting_lemond(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def unexpected_ready(_emit):
        raise AssertionError("unsupported translation must not contact lemond")

    monkeypatch.setattr(adapter, "_wait_ready_with_heartbeat", unexpected_ready)
    events = await _drive(adapter, SessionConfig(output_language="en"))

    assert len(events) == 1
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "translation_not_supported"


async def test_batch_session_emits_ready_final_and_done(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def ready(emit):
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        await emit(TranscriptionProgress(phase=PHASE_READY))

    wavs = []

    def transcribe(wav):
        wavs.append(wav)
        return "Hello from the NPU."

    monkeypatch.setattr(adapter, "_wait_ready_with_heartbeat", ready)
    monkeypatch.setattr(adapter, "_transcribe_wav", transcribe)
    monkeypatch.setattr(batch, "run_deferred_batch", _single_batch)
    pcm = np.zeros(16_000, dtype=np.int16).tobytes()
    events = await _drive(adapter, chunks=[PcmChunk(pcm, LEMOND_FORMAT)])

    assert [type(event) for event in events] == [
        TranscriptionProgress,
        TranscriptionProgress,
        TranscriptionFinal,
        TranscriptionDone,
    ]
    assert events[-2].text == "Hello from the NPU."
    assert events[-1].text == "Hello from the NPU."
    assert len(wavs) == 1
    assert wavs[0].startswith(b"RIFF")
    assert wavs[0][8:12] == b"WAVE"


async def test_empty_transcript_emits_done_without_an_empty_final(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def ready(_emit):
        pass

    monkeypatch.setattr(adapter, "_wait_ready_with_heartbeat", ready)
    monkeypatch.setattr(adapter, "_transcribe_wav", lambda _wav: "")
    monkeypatch.setattr(batch, "run_deferred_batch", _single_batch)
    events = await _drive(adapter)

    assert [type(event) for event in events] == [TranscriptionDone]
    assert events[0].text == ""


async def test_batch_regions_preserve_spacing_for_latin_and_cjk(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def two_batches(_audio, _emit, decode, on_commit):
        for offset in (0.0, 1.0):
            hypothesis = decode(np.zeros(16_000, dtype=np.float32), offset)
            await on_commit("".join(word.text for word in hypothesis.words), hypothesis.words)

    transcripts = iter(("Hello", "world"))
    monkeypatch.setattr(adapter, "_transcribe_wav", lambda _wav: next(transcripts))
    monkeypatch.setattr(batch, "run_deferred_batch", two_batches)
    assert await adapter._run_batch_session(_empty_audio(), _ignore_event) == "Hello world"

    transcripts = iter(("你好", "世界"))
    assert await adapter._run_batch_session(_empty_audio(), _ignore_event) == "你好世界"


async def _empty_audio():
    if False:
        yield PcmChunk(b"", LEMOND_FORMAT)


async def _ignore_event(_event):
    pass


async def test_readiness_emits_loading_heartbeats_then_ready(monkeypatch):
    adapter = LemondAdapter(api_key="secret")
    events = []

    async def delayed_thread(function):
        await lemond.asyncio.sleep(0.001)
        return function()

    async def emit(event):
        events.append(event)

    monkeypatch.setattr(adapter, "_wait_ready", lambda: None)
    monkeypatch.setattr(lemond.asyncio, "to_thread", delayed_thread)
    monkeypatch.setattr(lemond, "_LOAD_HEARTBEAT_SECONDS", 0.0)
    await adapter._wait_ready_with_heartbeat(emit)

    assert events[0].phase == PHASE_PREPARING
    assert events[-1].phase == PHASE_READY
    assert len(events) >= 3


def test_wait_ready_authenticates_health_then_preloads_the_model(monkeypatch):
    captured = {"requests": [], "timeouts": []}

    class Response:
        status = 200

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return b'{"status":"success"}'

    def urlopen(request, timeout):
        captured["requests"].append(request)
        captured["timeouts"].append(timeout)
        return Response()

    monkeypatch.setattr(urllib.request, "urlopen", urlopen)
    LemondAdapter(endpoint="http://127.0.0.1:19432/", api_key="secret")._wait_ready()

    health, load = captured["requests"]
    assert health.full_url == "http://127.0.0.1:19432/api/v1/health"
    assert health.method == "GET"
    assert health.headers["Authorization"] == "Bearer secret"
    assert load.full_url == "http://127.0.0.1:19432/api/v1/load"
    assert load.method == "POST"
    assert load.headers["Authorization"] == "Bearer secret"
    assert json.loads(load.data) == {"model_name": "whisper-v3-turbo-FLM"}
    assert captured["timeouts"] == [2.0, 120.0]


def test_wait_ready_timeout_reports_the_last_connection_error(monkeypatch):
    ticks = iter((0.0, 0.0, 1.0))
    monkeypatch.setattr(lemond, "_READY_TIMEOUT_SECONDS", 0.5)
    monkeypatch.setattr(lemond.time, "monotonic", lambda: next(ticks))
    monkeypatch.setattr(lemond.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(
        urllib.request,
        "urlopen",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(urllib.error.URLError("refused")),
    )

    try:
        LemondAdapter(api_key="secret")._wait_ready()
    except TimeoutError as exc:
        assert "refused" in str(exc)
    else:
        raise AssertionError("readiness timeout did not raise")


def test_wait_ready_reports_a_lemond_model_load_error(monkeypatch):
    class Response:
        status = 200

        def __init__(self, payload):
            self.payload = payload

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return self.payload

    calls = 0

    def urlopen(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        payload = b'{"status":"ok"}'
        if calls == 2:
            payload = b'{"status":"error","message":"NPU unavailable"}'
        return Response(payload)

    monkeypatch.setattr(urllib.request, "urlopen", urlopen)

    with pytest.raises(RuntimeError, match="NPU unavailable"):
        LemondAdapter(api_key="secret")._wait_ready()


def test_transcription_request_is_openai_multipart(monkeypatch):
    captured = {}

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return json.dumps({"text": "NPU transcript"}).encode()

    def urlopen(request, timeout):
        captured["request"] = request
        captured["timeout"] = timeout
        return Response()

    monkeypatch.setattr(urllib.request, "urlopen", urlopen)
    text = LemondAdapter(endpoint="http://127.0.0.1:1234", api_key="secret")._transcribe_wav(
        b"RIFFwave"
    )

    request = captured["request"]
    assert request.full_url == "http://127.0.0.1:1234/api/v1/audio/transcriptions"
    assert request.method == "POST"
    assert request.headers["Authorization"] == "Bearer secret"
    assert request.headers["Content-type"].startswith("multipart/form-data; boundary=")
    assert b'name="model"' in request.data
    assert b"whisper-v3-turbo-FLM" in request.data
    assert b'name="file"; filename="audio.wav"' in request.data
    assert b"RIFFwave" in request.data
    assert captured["timeout"] == 120.0
    assert text == "NPU transcript"


def test_transcription_response_requires_text(monkeypatch):
    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return json.dumps({"error": "bad audio"}).encode()

    monkeypatch.setattr(urllib.request, "urlopen", lambda *_args, **_kwargs: Response())

    try:
        LemondAdapter(api_key="secret")._transcribe_wav(b"RIFFwave")
    except RuntimeError as exc:
        assert "bad audio" in str(exc)
    else:
        raise AssertionError("response without transcript did not raise")


async def test_lemond_failure_is_a_terminal_inference_error(monkeypatch):
    adapter = LemondAdapter(api_key="secret")

    async def ready(emit):
        await emit(TranscriptionProgress(phase=PHASE_READY))

    def fail(_wav):
        raise RuntimeError("HRX device unavailable")

    monkeypatch.setattr(adapter, "_wait_ready_with_heartbeat", ready)
    monkeypatch.setattr(adapter, "_transcribe_wav", fail)
    monkeypatch.setattr(batch, "run_deferred_batch", _single_batch)
    pcm = np.zeros(1600, dtype=np.int16).tobytes()
    events = await _drive(adapter, chunks=[PcmChunk(pcm, LEMOND_FORMAT)])

    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "HRX device unavailable" in events[-1].message
