"""Offline unit tests for the Qwen3-ASR C/FFI adapter (T10a).

Neither the C shared library, a GPU, nor the weights are needed here: these
exercise the code that lives in the *adapter* rather than the runtime —
audio-format rejection, the IE639→Qwen language mapping, library-path
resolution, capabilities, idle-unload, the cold-load heartbeat, and the
buffer→final→done finalisation. The real decode is left to a hardware
integration check; stubbing the runtime here would assert call shapes, not
behaviour.

The format-rejection invariant is checked with Hypothesis over a wide input
space (the "symmetric across rate/channels/width" promise), like the other
adapters.
"""

import array
import asyncio

from hypothesis import assume, given
from hypothesis import strategies as st

from myna.core import (
    PHASE_PREPARING,
    PHASE_TRANSCRIBING,
    AudioFormat,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed import qwen as qwen_mod
from myna.testbed.qwen import (
    QWEN_FORMAT,
    QWEN_RATE,
    QwenCAdapter,
    _candidate_lib_paths,
    _qwen_language,
    _resolve_lib_path,
)

CANONICAL = (QWEN_RATE, 1, 2)  # 16 kHz mono S16LE — the only accepted format


# --- helpers ---------------------------------------------------------------


async def _drive_session(adapter, config, chunks):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for chunk in chunks:
            yield chunk

    await adapter.run_session(config, audio(), emit)
    return events


def _pcm_seconds(seconds: float, fmt: AudioFormat = QWEN_FORMAT) -> PcmChunk:
    return PcmChunk(data=b"\x00\x00" * int(seconds * fmt.sample_rate_hz), format=fmt)


def _instant(value=None):
    async def _load():
        return value

    return _load


# --- language mapping (adapter-owned quirk) --------------------------------


def test_qwen_language_maps_iso_codes_and_drops_region():
    assert _qwen_language("en") == "English"
    assert _qwen_language("en-GB") == "English"  # region subtag dropped
    assert _qwen_language("IT") == "Italian"  # case-insensitive


def test_qwen_language_none_and_unknown_fall_through_to_autodetect():
    assert _qwen_language(None) is None
    assert _qwen_language("") is None
    assert _qwen_language("xx") is None  # unmapped → auto-detect


# --- library-path resolution -----------------------------------------------


def test_candidate_lib_paths_ordering(monkeypatch):
    """Discovery order: explicit arg, then $QWEN_ASR_LIB, then the installed
    snap, then the bare name for the loader's own search path."""
    monkeypatch.setenv("QWEN_ASR_LIB", "/from/env.so")
    candidates = _candidate_lib_paths("/explicit.so")
    assert candidates[0] == "/explicit.so"
    assert candidates[1] == "/from/env.so"
    assert candidates[-1] == "libqwen_asr.so"
    assert any(c.startswith("/snap/qwen/") for c in candidates)
    # No explicit/env: still auto-discovers (snap + loader path), no manual setup.
    monkeypatch.delenv("QWEN_ASR_LIB")
    assert _candidate_lib_paths(None)[-1] == "libqwen_asr.so"
    assert "/explicit.so" not in _candidate_lib_paths(None)


def test_resolve_lib_path_returns_first_existing(monkeypatch, tmp_path):
    """An explicit arg only wins if it exists on disk; a bogus path falls through
    to discovery rather than being handed to a doomed CDLL()."""
    monkeypatch.delenv("QWEN_ASR_LIB", raising=False)
    real = tmp_path / "libqwen_asr.so"
    real.write_bytes(b"")
    assert _resolve_lib_path(str(real)) == str(real)
    # Nonexistent explicit path: skipped; falls back to the bare name (or an
    # installed snap) rather than resolving to the missing file.
    assert _resolve_lib_path("/does/not/exist.so") != "/does/not/exist.so"


# --- audio-format rejection (property) -------------------------------------


@given(
    rate=st.integers(min_value=1, max_value=192_000),
    channels=st.integers(min_value=1, max_value=8),
    width=st.integers(min_value=1, max_value=4),
)
def test_c_rejects_any_noncanonical_format(rate, channels, width):
    assume((rate, channels, width) != CANONICAL)
    fmt = AudioFormat(sample_rate_hz=rate, channels=channels, sample_width_bytes=width)
    events = asyncio.run(
        _drive_session(QwenCAdapter("/model"), SessionConfig(audio_format=fmt), [])
    )
    assert len(events) == 1  # rejected before the model loads
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


async def test_c_canonical_format_is_accepted(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    monkeypatch.setattr(adapter, "_transcribe", lambda pcm, config: "")
    events = await _drive_session(adapter, SessionConfig(audio_format=QWEN_FORMAT), [])
    assert not any(isinstance(e, TranscriptionError) for e in events)


# --- capabilities + candidate ----------------------------------------------


def test_c_capabilities_describe_multilingual_punctuating_model():
    caps = QwenCAdapter("/snap/qwen/components/3/Qwen3-ASR-0.6B").capabilities()
    assert caps.models == ("qwen-Qwen3-ASR-0.6B",)  # leaf name, not full path
    assert len(caps.languages) == 30 and "en" in caps.languages
    assert caps.punctuation is True
    assert caps.translation is False
    assert QWEN_FORMAT in caps.input_formats


def test_c_candidate_labels_engine_as_c_cpu():
    cand = QwenCAdapter("/x/Qwen3-ASR-1.7B").candidate
    assert cand.engine == "qwen-c-cpu"
    assert cand.model == "qwen-Qwen3-ASR-1.7B"
    assert cand.streaming_strategy == "commit-on-finalize"


# --- idle-unload (frees the C context) -------------------------------------


class _FakeLib:
    def __init__(self):
        self.freed = []

    def free(self, ctx):
        self.freed.append(ctx)


async def test_c_unload_frees_context_and_is_idempotent():
    adapter = QwenCAdapter("/model")
    lib = _FakeLib()
    adapter._lib, adapter._ctx = lib, 0xC0FFEE
    await adapter.unload()
    assert adapter._ctx is None
    assert lib.freed == [0xC0FFEE]  # the C-side context was released exactly once
    await adapter.unload()  # idempotent: no second free, must not raise
    assert lib.freed == [0xC0FFEE]


# --- cold-load heartbeat ----------------------------------------------------


async def _collect_heartbeat(adapter):
    events = []

    async def emit(event):
        events.append(event)

    await adapter._load_model_with_heartbeat(emit)
    return events


async def test_c_heartbeat_ticks_during_slow_load(monkeypatch):
    monkeypatch.setattr(qwen_mod, "_LOAD_HEARTBEAT_SECONDS", 0.02)
    adapter = QwenCAdapter("/model")

    async def slow_load():
        await asyncio.sleep(0.1)

    monkeypatch.setattr(adapter, "_load_model", slow_load)
    events = await _collect_heartbeat(adapter)
    assert all(isinstance(e, TranscriptionProgress) for e in events)
    assert all(e.phase == PHASE_PREPARING for e in events)
    assert len(events) >= 3


async def test_c_heartbeat_emits_once_when_warm(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    events = await _collect_heartbeat(adapter)
    assert events == [TranscriptionProgress(phase=PHASE_PREPARING)]


# --- run_session finalisation ----------------------------------------------


async def test_c_empty_audio_finalises_with_empty_done(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    events = await _drive_session(adapter, SessionConfig(), [])
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""
    assert not any(isinstance(e, TranscriptionFinal) for e in events)


async def test_c_buffered_audio_becomes_one_final_then_done(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    monkeypatch.setattr(adapter, "_transcribe", lambda pcm, config: "  Hello world.  ")
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [e.text for e in finals] == ["Hello world."]
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == "Hello world."


async def test_c_whitespace_only_transcript_emits_no_final(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    monkeypatch.setattr(adapter, "_transcribe", lambda pcm, config: "   \n ")
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    assert not any(isinstance(e, TranscriptionFinal) for e in events)
    assert events[-1].text == ""


async def test_c_progress_ticks_once_per_second_of_buffered_audio(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())
    monkeypatch.setattr(adapter, "_transcribe", lambda pcm, config: "ok")
    chunks = [_pcm_seconds(1.0) for _ in range(3)]
    events = await _drive_session(adapter, SessionConfig(), chunks)
    transcribing = [
        e for e in events if isinstance(e, TranscriptionProgress) and e.phase == PHASE_TRANSCRIBING
    ]
    assert len(transcribing) == 3


async def test_c_decode_failure_surfaces_as_error_event(monkeypatch):
    adapter = QwenCAdapter("/model")
    monkeypatch.setattr(adapter, "_load_model", _instant())

    def boom(pcm, config):
        raise RuntimeError("bad weights")

    monkeypatch.setattr(adapter, "_transcribe", boom)
    events = await _drive_session(adapter, SessionConfig(), [_pcm_seconds(0.1)])
    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "RuntimeError" in events[-1].message


async def test_c_missing_runtime_library_surfaces_actionable_error(monkeypatch):
    """A missing libqwen_asr.so must not read as an opaque OSError: distinct code
    plus a message that says where we looked and how to fix it (no manual
    LD_LIBRARY_PATH needed to understand the failure)."""
    monkeypatch.delenv("QWEN_ASR_LIB", raising=False)
    # Point at a library that definitely doesn't exist so real snap discovery
    # can't accidentally satisfy the load.
    adapter = QwenCAdapter("/model", lib_path="/nonexistent/libqwen_asr.so")
    monkeypatch.setattr(
        qwen_mod, "_resolve_lib_path", lambda explicit: "/nonexistent/libqwen_asr.so"
    )
    adapter._lib_path = "/nonexistent/libqwen_asr.so"
    events = await _drive_session(
        adapter, SessionConfig(audio_format=QWEN_FORMAT), [_pcm_seconds(0.1)]
    )
    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "runtime_library_missing"
    assert "libqwen_asr.so" in events[-1].message
    assert "QWEN_ASR_LIB" in events[-1].message


# --- int16 → float32 conversion (the FFI buffer contract) ------------------


def test_transcribe_builds_normalised_float_buffer(monkeypatch):
    # The C entry point expects normalised mono float32; verify the stdlib
    # conversion the adapter feeds it (no numpy), capturing what reaches the lib.
    captured = {}

    class _Lib:
        def set_force_language(self, ctx, lang):
            captured["lang"] = lang

        def set_prompt(self, ctx, prompt):
            captured["prompt"] = prompt

        def transcribe_audio(self, ctx, samples):
            captured["samples"] = list(samples)
            return "ok"

    adapter = QwenCAdapter("/model")
    adapter._lib, adapter._ctx = _Lib(), 1
    pcm = array.array("h", [0, 32767, -32768, 16384]).tobytes()
    out = adapter._transcribe(pcm, SessionConfig(language="en", prompt="hi"))
    assert out == "ok"
    assert captured["lang"] == "English"
    assert captured["prompt"] == "hi"
    s = captured["samples"]
    assert s[0] == 0.0
    assert abs(s[1] - 32767 / 32768.0) < 1e-6
    assert s[2] == -1.0  # -32768/32768
    assert abs(s[3] - 0.5) < 1e-6
