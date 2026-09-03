"""Offline unit tests for the Audio8-ASR-0.1B adapter (feature 010).

No onnxruntime, no staged model, no weights needed: these exercise the code
that lives in the *adapter* rather than the publisher's runtime — constructor
validation, capabilities, the prompt-pinning seam, the silence gate, the 30 s
cap rejection, output sanitization, WAV wrapping, and model-dir discovery. The
real decode is left to the quickstart scenarios and spikes (tasks.md T003–T005);
stubbing the engine here asserts call shapes and gate behavior, not model
behavior.

The engine is loaded by importlib from the staged dir at ``_load_model`` time
(lazy, research.md Decision 2), so every test that touches load/session
monkeypatches ``myna.testbed.audio8._load_runtime`` with a stub module — the
weights-free guarantee behind SC-007.
"""

from __future__ import annotations

import types
import wave

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import (
    AudioFormat,
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
)
from myna.testbed import audio8 as audio8_mod
from myna.testbed.audio8 import (
    AUDIO8_FORMAT,
    AUDIO8_RATE,
    Audio8Adapter,
    _default_model_dir,
    _strip_residual,
    _to_wav_bytes,
)

CANONICAL = (AUDIO8_RATE, 1, 2)  # 16 kHz mono S16LE — the only accepted format


# --- helpers ---------------------------------------------------------------


class _FakeEngine:
    """Minimal stand-in for the publisher's ``OnnxCacheAsrEngine``."""

    def __init__(
        self,
        bundle_dir,
        *,
        provider=None,
        intra_op_num_threads=None,
        cache_precision="int8",
        audio_precision=None,
    ):
        self.bundle_dir = bundle_dir
        self.provider = provider
        self.intra_op_num_threads = intra_op_num_threads
        self.cache_precision = cache_precision
        self.audio_precision = audio_precision
        self.max_audio_seconds = 30.0
        self.cache_graph = {"max_total_len": 512}
        self.prompt_merge_factor = 4

        class _FeatureExtractor:
            hop_length = 160

        self.feature_extractor = _FeatureExtractor()
        self.metadata = {
            "tokens": {
                "user_token": "<|im_start|>user",
                "bos_audio_token": "<|begin_of_audio|>",
                "eos_audio_token": "<|end_of_audio|>",
                "audio_token": "<|audio|>",
                "assistant_token": "<|im_start|>assistant",
            },
            "response_prefix": "",
        }
        self.calls = []
        self._text = "hello world"

    def _build_prompt(self, audio_token_count, language=None):
        return "base prompt"

    def transcribe(self, audio_bytes, *, language=None, max_new_tokens=128, hotwords=None):
        self.calls.append(dict(language=language, max_new_tokens=max_new_tokens, hotwords=hotwords))
        return {"text": self._text, "raw": self._text}


def _stub_runtime(monkeypatch, engine_cls=_FakeEngine):
    """Replace the importlib load with a stub module exposing the engine."""
    module = types.ModuleType(audio8_mod._MODULE_NAME)
    module.OnnxCacheAsrEngine = engine_cls
    monkeypatch.setattr(audio8_mod, "_load_runtime", lambda model_dir: module)
    return module


def _staged_model(tmp_path):
    """A staged dir the way dev/fetch_audio8_model.py leaves it."""
    snapshot = tmp_path / "snapshots" / "abc123"
    (snapshot / "model_bundle").mkdir(parents=True, exist_ok=True)
    (snapshot / "asr_onnx_runtime.py").write_text("")
    (snapshot / "model_bundle" / "metadata.json").write_text("{}")
    return snapshot


async def _drive_session(adapter, config, chunks):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for chunk in chunks:
            yield chunk

    await adapter.run_session(config, audio(), emit)
    return events


def _pcm(seconds: float) -> PcmChunk:
    return PcmChunk(data=b"\x00\x00" * int(seconds * AUDIO8_RATE), format=AUDIO8_FORMAT)


def _loaded_adapter(monkeypatch, tmp_path, **kwargs):
    """Adapter whose engine is already resident (bypasses warm-up noise)."""
    _stub_runtime(monkeypatch)
    adapter = Audio8Adapter(str(_staged_model(tmp_path)), **kwargs)
    engine = _FakeEngine(str(_staged_model(tmp_path)))
    adapter._engine = engine
    adapter._max_audio_seconds = 30.0
    # Mirror what _load_model records for the max_new_tokens clamp.
    adapter._max_total_len = int(engine.cache_graph["max_total_len"])
    adapter._prompt_merge_factor = engine.prompt_merge_factor
    adapter._hop_length = engine.feature_extractor.hop_length
    return adapter


# --- constructor validation -------------------------------------------------


def test_rejects_unsupported_language():
    with pytest.raises(ValueError, match="auto"):
        Audio8Adapter(language="it")  # selection is auto-only (spike T004)


def test_accepts_auto_language():
    Audio8Adapter(language="auto")  # must not raise


def test_rejects_precision():
    with pytest.raises(ValueError, match="cache_precision must be"):
        Audio8Adapter(cache_precision="int2")
    with pytest.raises(ValueError, match="audio_precision must be"):
        Audio8Adapter(audio_precision="fp16")


def test_rejects_unknown_device():
    with pytest.raises(ValueError, match="device must be"):
        Audio8Adapter(device="tpu")


def test_candidate_engine_reflects_device():
    assert Audio8Adapter().candidate.engine == "audio8-onnx-cpu"
    assert Audio8Adapter(device="cuda").candidate.engine == "audio8-onnx-cuda"


# --- capabilities + candidate ----------------------------------------------


def test_capabilities_advertise_auto_only_selection_and_formats():
    caps = Audio8Adapter().capabilities()
    assert caps.models == ("audio8-asr-0.1b",)
    # selection is auto-only (prompt pinning inert, spike T004); the model
    # still recognizes 7 languages under auto-detection.
    assert caps.languages == ("auto",)
    assert caps.punctuation is True  # spike-confirmed native punctuation (T005)
    assert caps.translation is False
    assert caps.input_formats == (AUDIO8_FORMAT,)


def test_capabilities_reflect_punctuation_override():
    assert Audio8Adapter(punctuation=False).capabilities().punctuation is False


def test_candidate_labels_engine_and_strategy():
    cand = Audio8Adapter().candidate
    assert cand.model == "audio8-asr-0.1b"
    assert cand.engine == "audio8-onnx-cpu"
    assert cand.streaming_strategy == "commit-on-finalize"


def test_adapter_is_batch_only():
    assert Audio8Adapter().streaming is False  # FR-004; model has no streaming


# --- output sanitization (FR-005, SC-006) ----------------------------------


def test_strip_residual_removes_language_prefix_and_tags():
    assert _strip_residual("language English <|foo|> hello  world") == "hello world"


def test_strip_residual_of_empty_is_empty():
    assert _strip_residual("") == ""
    assert _strip_residual("   ") == ""


def test_strip_residual_collapses_whitespace():
    assert _strip_residual("a\n\t  b") == "a b"


# --- WAV wrapping -----------------------------------------------------------


def test_to_wav_bytes_produces_parseable_16k_mono_s16le_wav():
    import io

    pcm = (np.arange(32000, dtype=np.int16)).tobytes()  # 1 s of s16le
    wav_bytes = _to_wav_bytes(pcm)
    with wave.open(io.BytesIO(wav_bytes), "rb") as w:
        assert w.getnchannels() == 1
        assert w.getsampwidth() == 2
        assert w.getframerate() == AUDIO8_RATE
        assert w.readframes(w.getnframes()) == pcm


# --- model-dir discovery ----------------------------------------------------


def test_default_model_dir_finds_staged_snapshot(monkeypatch, tmp_path):
    # _staged_model lays out .../snapshots/abc123 under tmp_path, but the
    # discovery glob expects hub/models--Audio8--.../snapshots/*.
    hub = tmp_path / "hub" / "models--Audio8--Audio8-ASR-0.1B-onnx-runtime" / "snapshots" / "xyz"
    hub.mkdir(parents=True)
    (hub / "asr_onnx_runtime.py").write_text("")
    (hub / "model_bundle").mkdir()
    (hub / "model_bundle" / "metadata.json").write_text("{}")
    monkeypatch.setenv("HF_HOME", str(tmp_path))
    assert _default_model_dir() == str(hub)


def test_default_model_dir_honors_env_override(monkeypatch, tmp_path):
    monkeypatch.setenv("AUDIO8_MODEL_DIR", str(tmp_path / "custom"))
    monkeypatch.setenv("HF_HOME", str(tmp_path))  # must be ignored when override is set
    assert _default_model_dir() == str(tmp_path / "custom")


def test_default_model_dir_ignores_snapshot_without_runtime(monkeypatch, tmp_path):
    hub = tmp_path / "hub" / "models--Audio8--Audio8-ASR-0.1B-onnx-runtime" / "snapshots" / "xyz"
    hub.mkdir(parents=True)
    (hub / "model_bundle").mkdir()
    (hub / "model_bundle" / "metadata.json").write_text("{}")
    monkeypatch.setenv("HF_HOME", str(tmp_path))
    with pytest.raises(FileNotFoundError):
        _default_model_dir()


def test_default_model_dir_error_names_the_fetch_script(monkeypatch, tmp_path):
    monkeypatch.setenv("HF_HOME", str(tmp_path))
    with pytest.raises(FileNotFoundError, match="dev/fetch_audio8_model.py"):
        _default_model_dir()


# --- load lifecycle (engine constructed directly; prompt seam removed T004) --


async def test_load_model_constructs_engine_from_staged_dir(monkeypatch, tmp_path):
    _stub_runtime(monkeypatch)
    adapter = Audio8Adapter(str(_staged_model(tmp_path)))
    engine = await adapter._load_model()
    assert isinstance(engine, _FakeEngine)
    # A small pool, not the machine's width: omitting this hands sizing to ORT
    # (and buys pinning), but measured 16% slower than 4 - T65.
    assert engine.intra_op_num_threads == 4
    assert adapter._max_audio_seconds == 30.0


async def test_load_model_is_idempotent(monkeypatch, tmp_path):
    _stub_runtime(monkeypatch)
    adapter = Audio8Adapter(str(_staged_model(tmp_path)))
    assert await adapter._load_model() is await adapter._load_model()


async def test_load_model_of_missing_dir_raises(monkeypatch):
    _stub_runtime(monkeypatch)
    with pytest.raises(FileNotFoundError):
        await Audio8Adapter("/nonexistent")._load_model()


async def test_cuda_device_fails_fast_without_cuda_provider(monkeypatch, tmp_path):
    """FR-020: GPU engine must not silently fall back to CPU."""
    module = _stub_runtime(monkeypatch)
    fake_ort = types.SimpleNamespace(get_available_providers=lambda: ["CPUExecutionProvider"])
    module.ort = fake_ort
    adapter = Audio8Adapter(str(_staged_model(tmp_path)), device="cuda")
    with pytest.raises(RuntimeError, match="CUDAExecutionProvider"):
        await adapter._load_model()


async def test_cuda_device_selects_cuda_provider(monkeypatch, tmp_path):
    module = _stub_runtime(monkeypatch)
    fake_ort = types.SimpleNamespace(
        get_available_providers=lambda: ["CUDAExecutionProvider", "CPUExecutionProvider"]
    )
    module.ort = fake_ort
    adapter = Audio8Adapter(str(_staged_model(tmp_path)), device="cuda")
    engine = await adapter._load_model()
    assert engine.provider == "CUDAExecutionProvider"


async def test_unload_releases_engine_and_is_idempotent(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    await adapter.unload()
    assert adapter._engine is None
    await adapter.unload()
    assert adapter._engine is None


# --- session: format rejection (FR-002) -------------------------------------


async def test_rejects_wrong_sample_rate(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    config = SessionConfig(
        audio_format=AudioFormat(sample_rate_hz=8000, channels=1, sample_width_bytes=2)
    )
    events = await _drive_session(adapter, config, [])
    assert events and isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


async def test_rejects_stereo(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    config = SessionConfig(
        audio_format=AudioFormat(sample_rate_hz=AUDIO8_RATE, channels=2, sample_width_bytes=2)
    )
    events = await _drive_session(adapter, config, [])
    assert events and isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


# --- session: unbounded audio via chunk-and-stitch (FR-009 amended) ---------


def test_decode_chunks_and_stitches_long_audio(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    adapter._max_audio_seconds = 1.0  # 1 s chunks
    adapter._engine._text = "A"
    loud = b"\x10\x00" * (AUDIO8_RATE * 3)  # 3 s of loud audio
    assert adapter._decode(loud) == "A A A"


def test_decode_single_chunk_short_audio(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    adapter._max_audio_seconds = 30.0
    adapter._engine._text = "hello"
    loud = b"\x10\x00" * AUDIO8_RATE  # 1 s — single chunk
    assert adapter._decode(loud) == "hello"


async def test_long_audio_session_emits_stitched_final(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    adapter._max_audio_seconds = 1.0
    engine = adapter._engine
    engine._text = "chunk"
    config = SessionConfig(audio_format=AUDIO8_FORMAT)
    loud = PcmChunk(data=b"\x10\x00" * (AUDIO8_RATE * 3), format=AUDIO8_FORMAT)
    events = await _drive_session(adapter, config, [loud])
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert finals and finals[0].text == "chunk chunk chunk"
    # 3 real decodes (the warm-up call has max_new_tokens=8).
    real = [c for c in engine.calls if c["max_new_tokens"] != 8]
    assert len(real) == 3


# --- session: happy path + silence gate (FR-005, Decision 7) ----------------


async def test_session_emits_final_then_done(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    engine = adapter._engine
    config = SessionConfig(audio_format=AUDIO8_FORMAT)
    # Loud enough to pass the silence gate.
    loud = PcmChunk(data=b"\x10\x00\xf0\xff" * (AUDIO8_RATE // 2), format=AUDIO8_FORMAT)
    events = await _drive_session(adapter, config, [loud])
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    dones = [e for e in events if isinstance(e, TranscriptionDone)]
    assert finals and finals[0].text == "hello world"
    assert finals[0].disposition is Disposition.COMMITTED
    assert dones and dones[-1].text == "hello world"
    assert engine.calls, "engine must have been invoked"
    assert engine.calls[-1]["max_new_tokens"] == 256
    assert engine.calls[-1]["hotwords"] is None  # hotwords out of scope


async def test_silence_skips_decode_and_emits_empty_done(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    engine = adapter._engine
    config = SessionConfig(audio_format=AUDIO8_FORMAT)
    silence = _pcm(1.0)  # all zeros → RMS 0 → gated
    events = await _drive_session(adapter, config, [silence])
    assert not any(isinstance(e, TranscriptionFinal) for e in events)
    done = [e for e in events if isinstance(e, TranscriptionDone)]
    assert done and done[-1].text == ""
    # Warm-up (max_new_tokens=8) is the only call — the real decode (256) is gated.
    assert not any(c["max_new_tokens"] == 256 for c in engine.calls), (
        "silence must not reach the model"
    )


async def test_silence_gate_disabled_when_threshold_none(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path, silence_threshold=None)
    engine = adapter._engine
    config = SessionConfig(audio_format=AUDIO8_FORMAT)
    await _drive_session(adapter, config, [_pcm(1.0)])
    # Gate disabled → the real decode (max_new_tokens=256) must reach the model.
    assert any(c["max_new_tokens"] == 256 for c in engine.calls)


# --- decode sanitization through the stub -----------------------------------


async def test_decode_strips_residual_artifacts(monkeypatch, tmp_path):
    adapter = _loaded_adapter(monkeypatch, tmp_path)
    adapter._engine._text = "language English <|special|> punctuated."
    text = adapter._decode(b"\x10\x00" * 1000)
    assert text == "punctuated."


def test_strip_tags_alias_present_for_shared_sweep():
    # The funasr adapter exposes _strip_tags; audio8 exposes _strip_residual.
    # Keep the residual sweep importable for parity in coverage tooling.
    assert callable(audio8_mod._strip_residual)


# --- max_new_tokens clamp (FR-008, cache decoder 512-token budget) ----------


def _loaded_for_clamp():
    a = Audio8Adapter()
    a._max_total_len = 512
    a._prompt_merge_factor = 4
    a._hop_length = 160
    return a


def test_clamp_leaves_short_utterances_at_the_configured_cap():
    a = _loaded_for_clamp()
    # 1 s = 16000 samples -> ~12 audio tokens -> prompt ~28; budget leaves 484.
    assert a._clamp_max_new_tokens(16000) == 256


def test_clamp_reduces_cap_for_long_utterances():
    a = _loaded_for_clamp()
    # 20 s -> 250 audio tokens -> prompt ~266 -> budget leaves 246 < 256.
    assert a._clamp_max_new_tokens(20 * 16000) == 246


def test_clamp_at_30s_cap_stays_positive():
    a = _loaded_for_clamp()
    # 30 s -> 375 audio tokens -> prompt ~391 -> budget leaves 121.
    assert a._clamp_max_new_tokens(30 * 16000) == 121
    assert a._clamp_max_new_tokens(30 * 16000) > 0


def test_clamp_before_load_returns_configured_cap():
    assert Audio8Adapter()._clamp_max_new_tokens(480000) == 256
