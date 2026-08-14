"""Offline unit tests for the FunASR / SenseVoice-Small adapter (feature 009).

Neither ONNX Runtime nor the staged model is needed: these exercise the code
that lives in the *adapter* rather than the runtime - constructor validation,
capabilities, model-dir discovery, quantization auto-detect, the cold-load
heartbeat, warm-up, tag stripping, the s16le->float32 buffer contract, and the
buffer->final->done finalisation. The real decode is left to the quickstart
hardware scenarios; stubbing SenseVoice here would assert call shapes, not
behaviour.

The format-rejection invariant is checked with Hypothesis over a wide input
space (the "symmetric across rate/channels/width" promise), like the other
adapters.
"""

import array
import asyncio
import sys
import types

import pytest
from hypothesis import assume, given
from hypothesis import strategies as st

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    PHASE_TRANSCRIBING,
    AudioFormat,
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed import funasr as funasr_mod
from myna.testbed.funasr import (
    FUNASR_FORMAT,
    FUNASR_RATE,
    FunasrAdapter,
    _default_model_dir,
    _strip_tags,
)

CANONICAL = (FUNASR_RATE, 1, 2)  # 16 kHz mono S16LE - the only accepted format
_BPE = "chn_jpn_yue_eng_ko_spectok.bpe.model"


# --- helpers ---------------------------------------------------------------


class _StubModel:
    """Stand-in for ``SenseVoiceSmall``: records every call (warm-up included)
    and replays a scripted output."""

    def __init__(self, output="hello"):
        self.calls = []
        self._output = output

    def __call__(self, samples, **kwargs):
        self.calls.append((samples, kwargs))
        return [self._output]


def _stub_load(adapter, model):
    """Replace ``_load_model`` the way the real one behaves: publish the model
    on ``self._model`` (warm-up reads it from there) and return it."""

    async def _load():
        adapter._model = model
        return model

    return _load


async def _drive_session(adapter, config, chunks):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for chunk in chunks:
            yield chunk

    await adapter.run_session(config, audio(), emit)
    return events


def _pcm_seconds(seconds: float, fmt: AudioFormat = FUNASR_FORMAT) -> PcmChunk:
    return PcmChunk(data=b"\x00\x00" * int(seconds * fmt.sample_rate_hz), format=fmt)


def _staged_model(tmp_path, *, quantized: bool = False):
    """A ModelScope cache laid out the way ``dev/fetch_funasr_model.py`` leaves it."""
    snapshot = tmp_path / "models" / "botaruibo--SenseVoiceSmall-onnx" / "snapshots" / "abc123"
    snapshot.mkdir(parents=True)
    (snapshot / _BPE).write_bytes(b"")
    (snapshot / ("model_quant.onnx" if quantized else "model.onnx")).write_bytes(b"")
    return snapshot


# --- constructor validation -------------------------------------------------


def test_rejects_unsupported_language():
    with pytest.raises(ValueError, match="language must be one of"):
        FunasrAdapter(language="de")  # not in SenseVoice's lid_dict


def test_rejects_unknown_textnorm():
    with pytest.raises(ValueError, match="textnorm must be"):
        FunasrAdapter(textnorm="itn")  # the flag is woitn/withitn


def test_accepts_every_advertised_language():
    for language in FunasrAdapter().capabilities().languages:
        FunasrAdapter(language=language)  # must not raise


# --- capabilities + candidate ----------------------------------------------


def test_capabilities_describe_multilingual_unpunctuated_model():
    caps = FunasrAdapter().capabilities()
    assert caps.models == ("sensevoice-small",)
    assert caps.languages == ("auto", "zh", "en", "yue", "ja", "ko")
    assert caps.punctuation is False  # FR-008: sherpa-compatible posture
    assert caps.translation is False
    assert caps.input_formats == (FUNASR_FORMAT,)


def test_candidate_labels_engine_as_onnx_cpu():
    cand = FunasrAdapter().candidate
    assert cand.engine == "funasr-onnx-cpu"
    assert cand.model == "sensevoice-small"
    assert cand.streaming_strategy == "commit-on-finalize"


def test_candidate_model_is_leaf_of_explicit_dir():
    # Leaf name, not the full path - and a trailing slash must not blank it.
    assert FunasrAdapter("/models/SenseVoiceSmall-onnx").candidate.model == ("SenseVoiceSmall-onnx")
    assert FunasrAdapter("/models/SenseVoiceSmall-onnx/").candidate.model == (
        "SenseVoiceSmall-onnx"
    )


def test_adapter_is_batch_only():
    assert FunasrAdapter().streaming is False  # FR-004


# --- tag stripping (FR-005, SC-006) ----------------------------------------


def test_strip_tags_removes_control_tags_and_trims():
    raw = "<|en|><|NEUTRAL|><|Speech|><|woitn|>hello world"
    assert _strip_tags(raw) == "hello world"


def test_strip_tags_of_tag_only_output_is_empty():
    # Silence decodes to tags alone; nothing must survive to the wire.
    assert _strip_tags("<|nospeech|><|woitn|>  ") == ""
    assert _strip_tags("") == ""


# --- model-dir discovery ----------------------------------------------------


def test_default_model_dir_finds_staged_snapshot(monkeypatch, tmp_path):
    snapshot = _staged_model(tmp_path)
    monkeypatch.setenv("MODELSCOPE_CACHE", str(tmp_path))
    assert _default_model_dir() == str(snapshot)


def test_default_model_dir_ignores_snapshot_without_tokenizer(monkeypatch, tmp_path):
    # A half-fetched snapshot must not be handed to SenseVoiceSmall.
    partial = tmp_path / "models" / "botaruibo--SenseVoiceSmall-onnx" / "snapshots" / "abc123"
    partial.mkdir(parents=True)
    monkeypatch.setenv("MODELSCOPE_CACHE", str(tmp_path))
    with pytest.raises(FileNotFoundError):
        _default_model_dir()


def test_default_model_dir_error_names_the_fetch_script(monkeypatch, tmp_path):
    """Nothing staged must read as an actionable message, not a bare ImportError
    from funasr_onnx's download fallback (constitution V: never fetch at
    session time)."""
    monkeypatch.setenv("MODELSCOPE_CACHE", str(tmp_path))
    with pytest.raises(FileNotFoundError, match="dev/fetch_funasr_model.py"):
        _default_model_dir()


# --- load: quantization auto-detect (FR-016) --------------------------------


def _fake_funasr_onnx(monkeypatch):
    """Install a stub ``funasr_onnx`` module capturing SenseVoiceSmall kwargs."""
    captured = {}

    def _sense_voice_small(**kwargs):
        captured.update(kwargs)
        return _StubModel()

    module = types.ModuleType("funasr_onnx")
    module.SenseVoiceSmall = _sense_voice_small
    monkeypatch.setitem(sys.modules, "funasr_onnx", module)
    return captured


async def test_load_prefers_int8_export_when_present(monkeypatch, tmp_path):
    captured = _fake_funasr_onnx(monkeypatch)
    snapshot = _staged_model(tmp_path, quantized=True)
    await FunasrAdapter(str(snapshot), num_threads=2)._load_model()
    assert captured["quantize"] is True
    assert captured["device_id"] == "-1"  # CPU inference
    assert captured["intra_op_num_threads"] == 2


async def test_load_falls_back_to_fp32_export(monkeypatch, tmp_path):
    captured = _fake_funasr_onnx(monkeypatch)
    snapshot = _staged_model(tmp_path, quantized=False)
    await FunasrAdapter(str(snapshot))._load_model()
    assert captured["quantize"] is False


async def test_load_is_idempotent(monkeypatch, tmp_path):
    _fake_funasr_onnx(monkeypatch)
    adapter = FunasrAdapter(str(_staged_model(tmp_path)))
    assert await adapter._load_model() is await adapter._load_model()


async def test_load_of_missing_dir_raises_before_the_runtime_sees_it(monkeypatch):
    _fake_funasr_onnx(monkeypatch)
    with pytest.raises(FileNotFoundError, match="model dir not found"):
        await FunasrAdapter("/nonexistent/sensevoice")._load_model()


# --- idle-unload (T27) ------------------------------------------------------


async def test_unload_releases_session_and_is_idempotent():
    adapter = FunasrAdapter()
    adapter._model = _StubModel()
    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idempotent: must not raise
    assert adapter._model is None


# --- cold-load heartbeat + warm-up (FR-009) ---------------------------------


async def _collect_load(adapter):
    events = []

    async def emit(event):
        events.append(event)

    await adapter._load_model_with_heartbeat(emit)
    return events


async def test_heartbeat_ticks_during_slow_load(monkeypatch):
    monkeypatch.setattr(funasr_mod, "_LOAD_HEARTBEAT_SECONDS", 0.02)
    adapter = FunasrAdapter()
    model = _StubModel()

    async def slow_load():
        await asyncio.sleep(0.1)
        adapter._model = model
        return model

    monkeypatch.setattr(adapter, "_load_model", slow_load)
    events = await _collect_load(adapter)
    assert all(isinstance(e, TranscriptionProgress) for e in events)
    assert [e.phase for e in events[:-1]] == [PHASE_PREPARING] * (len(events) - 1)
    assert len(events) >= 4  # preparing + >=2 heartbeats + ready
    assert events[-1].phase == PHASE_READY


async def test_ready_is_emitted_only_after_warm_up():
    """The client gates on `ready`; announcing it before the warm-up inference
    would hand the first real utterance the graph-optimization bill."""
    adapter = FunasrAdapter()
    order = []

    class _Recording(_StubModel):
        def __call__(self, samples, **kwargs):
            order.append("warm-up")
            return super().__call__(samples, **kwargs)

    adapter._load_model = _stub_load(adapter, _Recording())

    async def emit(event):
        order.append(event.phase)

    await adapter._load_model_with_heartbeat(emit)
    assert order[-2:] == ["warm-up", PHASE_READY]


async def test_warm_up_feeds_six_seconds_of_float32_noise():
    adapter = FunasrAdapter()
    model = _StubModel()
    adapter._load_model = _stub_load(adapter, model)
    await _collect_load(adapter)
    samples, _ = model.calls[0]
    assert samples.dtype == np.float32
    assert len(samples) == int(FUNASR_RATE * 6.0)
    assert samples.any()  # noise, not silence


async def test_warm_up_output_never_reaches_the_wire():
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel(output="<|nospeech|>warm"))
    events = await _collect_load(adapter)
    assert all(isinstance(e, TranscriptionProgress) for e in events)


# --- audio-format rejection (property) --------------------------------------


@given(
    rate=st.integers(min_value=1, max_value=192_000),
    channels=st.integers(min_value=1, max_value=8),
    width=st.integers(min_value=1, max_value=4),
)
def test_rejects_any_noncanonical_format(rate, channels, width):
    assume((rate, channels, width) != CANONICAL)
    fmt = AudioFormat(sample_rate_hz=rate, channels=channels, sample_width_bytes=width)
    events = asyncio.run(_drive_session(FunasrAdapter(), SessionConfig(audio_format=fmt), []))
    assert len(events) == 1  # rejected before the model loads
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


async def test_canonical_format_is_accepted():
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel())
    events = await _drive_session(adapter, SessionConfig(audio_format=FUNASR_FORMAT), [])
    assert not any(isinstance(e, TranscriptionError) for e in events)


# --- run_session finalisation -----------------------------------------------


async def test_buffered_audio_becomes_one_committed_final_then_done():
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel(output="<|en|><|NEUTRAL|>hello world"))
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [e.text for e in finals] == ["hello world"]  # tags stripped before the wire
    assert finals[0].disposition == Disposition.COMMITTED
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == "hello world"


async def test_empty_audio_finalises_with_empty_done():
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel())
    events = await _drive_session(adapter, SessionConfig(), [])
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""
    assert not any(isinstance(e, TranscriptionFinal) for e in events)


async def test_tag_only_transcript_emits_no_final():
    """Silence decodes to control tags alone. An empty final is not harmless:
    the harness counts it as a committed segment and dates time_to_first_final
    from it, so a silent clip would report a bogus TTFF."""
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel(output="<|nospeech|><|woitn|>"))
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    assert not any(isinstance(e, TranscriptionFinal) for e in events)
    assert events[-1].text == ""


async def test_progress_ticks_once_per_second_of_buffered_audio():
    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _StubModel())
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(1.0) for _ in range(3)])
    transcribing = [
        e for e in events if isinstance(e, TranscriptionProgress) and e.phase == PHASE_TRANSCRIBING
    ]
    assert len(transcribing) == 3


async def test_decode_failure_surfaces_as_error_event():
    class _Boom(_StubModel):
        def __call__(self, samples, **kwargs):
            if self.calls:  # let the warm-up through; fail the real decode
                raise RuntimeError("bad graph")
            return super().__call__(samples, **kwargs)

    adapter = FunasrAdapter()
    adapter._load_model = _stub_load(adapter, _Boom())
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "RuntimeError: bad graph" in events[-1].message


# --- decode: waveform + kwargs contract -------------------------------------


async def test_decode_passes_language_and_textnorm_through():
    adapter = FunasrAdapter(language="ja", textnorm="withitn")
    model = _StubModel()
    adapter._load_model = _stub_load(adapter, model)
    await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    _, kwargs = model.calls[-1]
    assert kwargs == {"language": "ja", "textnorm": "withitn"}


async def test_decode_receives_normalised_float32_samples():
    # SenseVoice expects normalised mono float32; verify the s16le conversion.
    adapter = FunasrAdapter()
    model = _StubModel()
    adapter._load_model = _stub_load(adapter, model)
    pcm = array.array("h", [0, 32767, -32768, 16384]).tobytes()
    await _drive_session(adapter, SessionConfig(), [PcmChunk(data=pcm, format=FUNASR_FORMAT)])
    samples, _ = model.calls[-1]
    assert samples.dtype == np.float32
    assert samples[0] == 0.0
    assert abs(samples[1] - 32767 / 32768.0) < 1e-6
    assert samples[2] == -1.0  # -32768/32768
    assert abs(samples[3] - 0.5) < 1e-6
