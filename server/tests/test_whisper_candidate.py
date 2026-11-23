"""Candidate metadata for the faster-whisper adapter (no model/extra needed).

Constructing the adapter and reading ``.candidate`` does not import
faster_whisper, so this runs in the default offline suite. It guards the
label normalisation that keeps result records readable when the snap loads
weights from a local CTranslate2 model-component directory (T15).
"""

import asyncio
import sys
import threading

import pytest
from hypothesis import assume, given
from hypothesis import strategies as st

from myna.core import AudioFormat, SessionConfig, TranscriptionError
from myna.testbed.whisper import WHISPER_RATE, FasterWhisperAdapter, _iso639_1

CANONICAL = (WHISPER_RATE, 1, 2)  # 16 kHz mono S16LE — the only accepted format


def test_iso639_1_drops_region_subtag():
    # faster-whisper rejects BCP-47 region tags; the corpus uses them.
    assert _iso639_1("en-GB") == "en"
    assert _iso639_1("de") == "de"
    assert _iso639_1(None) is None  # auto-detect


@pytest.mark.parametrize(
    ("tag", "code"), [("nb", "no"), ("nb-NO", "no"), ("jv", "jw"), ("fil", "tl")]
)
def test_iso639_1_uses_whispers_code_where_it_differs_from_bcp47(tag, code):
    # FLEURS nb_no, jv_id and fil_ph arrive as their BCP-47 tags; faster-whisper
    # rejects those and knows the languages as no, jw and tl.
    assert _iso639_1(tag) == code


def test_candidate_labels_a_bare_size():
    cand = FasterWhisperAdapter("small").candidate
    assert cand.model == "whisper-small"
    assert cand.engine == "faster-whisper-cpu"
    assert cand.streaming_strategy == "commit-on-finalize"


async def test_unload_drops_the_model():
    # idle-unload (T27); no real model needed — just the reference handling
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = object()
    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idempotent


def test_candidate_labels_a_component_directory_by_leaf():
    # snap passes --model $SNAP_COMPONENTS/model-small
    cand = FasterWhisperAdapter("/snap/whisper/components/42/model-small/", device="cuda").candidate
    assert cand.model == "whisper-model-small"  # leaf, not the absolute path
    assert cand.engine == "faster-whisper-cuda"


@given(
    rate=st.integers(min_value=1, max_value=192_000),
    channels=st.integers(min_value=1, max_value=8),
    width=st.integers(min_value=1, max_value=4),
)
def test_rejects_any_noncanonical_format(rate, channels, width):
    # Rejection happens before the model loads (audio-push: client owns
    # capture + conversion), so this needs neither faster-whisper nor a model.
    assume((rate, channels, width) != CANONICAL)
    fmt = AudioFormat(sample_rate_hz=rate, channels=channels, sample_width_bytes=width)

    async def drive():
        events = []

        async def emit(event):
            events.append(event)

        async def no_audio():
            for chunk in ():  # rejected before any chunk is read
                yield chunk

        await FasterWhisperAdapter("tiny").run_session(
            SessionConfig(audio_format=fmt), no_audio(), emit
        )
        return events

    events = asyncio.run(drive())
    assert len(events) == 1  # rejected outright, nothing else emitted
    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"


# ─── compute type ────────────────────────────────────────────────────────────


def _fake_ctranslate2(monkeypatch, supported):
    """CTranslate2's own answer, stubbed - the real one needs the whisper extra."""
    module = type(
        "M", (), {"get_supported_compute_types": staticmethod(lambda device: set(supported))}
    )
    monkeypatch.setitem(sys.modules, "ctranslate2", module)


def test_a_compute_type_the_device_cannot_do_is_refused(monkeypatch):
    """CTranslate2 rejects float16-on-CPU too, but only from inside the model
    constructor and without naming what it would have taken - which is a sweep
    cell dying on its first clip, or a daemon that starts and then fails on
    first dictation."""
    _fake_ctranslate2(monkeypatch, {"float32", "int8", "int8_float32"})
    adapter = FasterWhisperAdapter("tiny", device="cpu", compute_type="float16")
    with pytest.raises(ValueError, match="not available on device 'cpu'"):
        adapter._check_compute_type()


def test_a_supported_compute_type_passes(monkeypatch):
    _fake_ctranslate2(monkeypatch, {"float32", "int8", "int8_float32"})
    FasterWhisperAdapter("tiny", device="cpu", compute_type="int8")._check_compute_type()


@pytest.mark.parametrize("deferral", ["default", "auto"])
def test_a_deferral_is_not_a_request_and_is_left_to_ctranslate2(monkeypatch, deferral):
    def explode(device):
        raise AssertionError("a deferral must not be checked against the supported set")

    monkeypatch.setitem(
        sys.modules,
        "ctranslate2",
        type("M", (), {"get_supported_compute_types": staticmethod(explode)}),
    )
    FasterWhisperAdapter("tiny", compute_type=deferral)._check_compute_type()


# ─── GPU unavailable ─────────────────────────────────────────────────────────


class _RecordingWhisperModel:
    """faster_whisper.WhisperModel, recording where it was asked to load."""

    loads: list[tuple[str, str]] = []
    sources: list[tuple[str, str | None]] = []
    fails_on: tuple[str, ...] = ()
    decode_fails_on: tuple[str, ...] = ()
    cuda_decodes_before_failing: int | None = None
    decodes: list[str] = []

    def __init__(self, model_size, *, device, compute_type, download_root):
        self.loads.append((device, compute_type))
        self.sources.append((model_size, download_root))
        if device in self.fails_on:
            raise RuntimeError(f"failed on {device}")
        self.device = device
        self.model = type("CT2", (), {"compute_type": compute_type})()

    def transcribe(self, samples, **kwargs):
        def segments():
            self.decodes.append(self.device)
            if self.device in self.decode_fails_on:
                raise RuntimeError("cuBLAS failed with status CUBLAS_STATUS_ARCH_MISMATCH")
            limit = self.cuda_decodes_before_failing
            if self.device == "cuda" and limit is not None and self.decodes.count("cuda") > limit:
                raise RuntimeError("CUDA failed with error out of memory")
            yield from ()

        return segments(), type("Info", (), {"language": "en"})()


_TQDM_LOCKS: list = []


def _fake_whisper_stack(monkeypatch, cuda_devices):
    _TQDM_LOCKS.clear()
    tqdm = type("tqdm", (), {"set_lock": staticmethod(_TQDM_LOCKS.append)})
    monkeypatch.setitem(sys.modules, "tqdm", type("T", (), {"tqdm": tqdm}))
    module = type(
        "M",
        (),
        {
            "get_cuda_device_count": staticmethod(lambda: cuda_devices),
            "get_supported_compute_types": staticmethod(
                lambda device: {"float16", "float32", "int8"} if device == "cuda" else {"float32"}
            ),
        },
    )
    monkeypatch.setitem(sys.modules, "ctranslate2", module)
    _RecordingWhisperModel.loads = []
    _RecordingWhisperModel.sources = []
    _RecordingWhisperModel.fails_on = ()
    _RecordingWhisperModel.decode_fails_on = ()
    _RecordingWhisperModel.cuda_decodes_before_failing = None
    _RecordingWhisperModel.decodes = []
    monkeypatch.setitem(
        sys.modules, "faster_whisper", type("F", (), {"WhisperModel": _RecordingWhisperModel})
    )


def test_a_cuda_engine_without_a_usable_gpu_loads_on_the_cpu(monkeypatch):
    """The reporter's whisper engine failed every utterance: \"CUDA driver
    version is insufficient for CUDA runtime version\", then a float16 refusal.
    CTranslate2 sees no CUDA device then; load on the CPU in its own type."""
    _fake_whisper_stack(monkeypatch, cuda_devices=0)
    adapter = FasterWhisperAdapter("small", device="cuda", compute_type="float16")

    asyncio.run(adapter._load_model())

    assert _RecordingWhisperModel.loads == [("cpu", "default")]
    assert adapter.capabilities().runtime["device"] == "cpu"


def test_a_gpu_ctranslate2_cannot_start_on_loads_on_the_cpu(monkeypatch):
    """CTranslate2 counts a device it then fails to run on (#56): construction
    throws. Serve on the CPU in its own type."""
    _fake_whisper_stack(monkeypatch, cuda_devices=1)
    _RecordingWhisperModel.fails_on = ("cuda",)
    adapter = FasterWhisperAdapter(
        "small", device="cuda", compute_type="float16", download_root="/models"
    )

    asyncio.run(adapter._load_model())

    assert _RecordingWhisperModel.loads == [("cuda", "float16"), ("cpu", "default")]
    assert _RecordingWhisperModel.sources == [("small", "/models")] * 2
    assert adapter.capabilities().runtime["device"] == "cpu"


def test_a_gpu_that_fails_at_its_first_decode_loads_on_the_cpu(monkeypatch):
    """CTranslate2 creates its cuBLAS handle at the first GEMM, so on a card
    cuBLAS has no kernels for, construction passes and every transcribe fails."""
    _fake_whisper_stack(monkeypatch, cuda_devices=1)
    _RecordingWhisperModel.decode_fails_on = ("cuda",)
    adapter = FasterWhisperAdapter("small", device="cuda", compute_type="float16")

    asyncio.run(adapter._load_model())

    assert _RecordingWhisperModel.loads == [("cuda", "float16"), ("cpu", "default")]
    assert adapter.capabilities().runtime["device"] == "cpu"


def test_a_model_that_fails_on_the_cpu_too_is_not_a_gpu_fault(monkeypatch):
    _fake_whisper_stack(monkeypatch, cuda_devices=1)
    _RecordingWhisperModel.fails_on = ("cuda", "cpu")
    adapter = FasterWhisperAdapter("small", device="cuda", compute_type="float16")

    with pytest.raises(RuntimeError, match="failed on cpu"):
        asyncio.run(adapter._load_model())


def test_a_cpu_engine_load_failure_is_not_retried(monkeypatch):
    _fake_whisper_stack(monkeypatch, cuda_devices=0)
    _RecordingWhisperModel.fails_on = ("cpu",)
    adapter = FasterWhisperAdapter("small", device="cpu", compute_type="float32")

    with pytest.raises(RuntimeError, match="failed on cpu"):
        asyncio.run(adapter._load_model())
    assert len(_RecordingWhisperModel.loads) == 1


def test_a_usable_gpu_loads_as_asked(monkeypatch):
    _fake_whisper_stack(monkeypatch, cuda_devices=1)
    adapter = FasterWhisperAdapter("small", device="cuda", compute_type="float16")

    asyncio.run(adapter._load_model())

    assert _RecordingWhisperModel.loads == [("cuda", "float16")]
    assert adapter.capabilities().runtime["device"] == "cuda"


@pytest.mark.parametrize("streaming", [False, True])
def test_a_gpu_that_fails_mid_session_finishes_it_on_the_cpu(monkeypatch, streaming):
    """The warm-up decode passed, then a later one ran out of memory: decode
    it on the CPU, and every one after."""
    import numpy as np

    from myna.core import PcmChunk, TranscriptionDone

    _fake_whisper_stack(monkeypatch, cuda_devices=1)
    _RecordingWhisperModel.cuda_decodes_before_failing = 1
    adapter = FasterWhisperAdapter(
        "small", device="cuda", compute_type="float16", streaming=streaming
    )
    fmt = AudioFormat(sample_rate_hz=WHISPER_RATE, channels=1, sample_width_bytes=2)
    tone = (np.sin(np.arange(3 * WHISPER_RATE) * 0.05) * 8000).astype(np.int16).tobytes()

    async def drive():
        events = []

        async def emit(event):
            events.append(event)

        async def audio():
            yield PcmChunk(data=tone, format=fmt)

        await adapter.run_session(SessionConfig(audio_format=fmt), audio(), emit)
        return events

    events = asyncio.run(drive())

    assert not [e for e in events if isinstance(e, TranscriptionError)]
    assert [e for e in events if isinstance(e, TranscriptionDone)]
    assert _RecordingWhisperModel.loads == [("cuda", "float16"), ("cpu", "default")]
    assert _RecordingWhisperModel.decodes[:3] == ["cuda", "cuda", "cpu"]
    assert set(_RecordingWhisperModel.decodes[3:]) <= {"cpu"}
    assert adapter.capabilities().runtime["device"] == "cpu"
    assert adapter.gpu_unavailable


def test_a_session_on_a_cpu_fallback_says_so_when_ready(monkeypatch):
    from myna.core import PHASE_READY, WARNING_GPU_UNAVAILABLE
    from myna.server.lifecycle import GPU_UNAVAILABLE_MESSAGE

    _fake_whisper_stack(monkeypatch, cuda_devices=0)
    adapter = FasterWhisperAdapter("small", device="cuda", compute_type="float16")

    async def drive():
        events = []

        async def emit(event):
            events.append(event)

        async def no_audio():
            for chunk in ():
                yield chunk

        fmt = AudioFormat(sample_rate_hz=WHISPER_RATE, channels=1, sample_width_bytes=2)
        await adapter.run_session(SessionConfig(audio_format=fmt), no_audio(), emit)
        return events

    warned = [e for e in asyncio.run(drive()) if getattr(e, "warning", None)]
    assert [(e.phase, e.warning, e.warning_code) for e in warned] == [
        (PHASE_READY, GPU_UNAVAILABLE_MESSAGE, WARNING_GPU_UNAVAILABLE)
    ]


def test_tqdm_gets_a_thread_lock_before_the_model_loads(monkeypatch):
    """faster-whisper builds a disabled tqdm bar per decode, and tqdm's default
    lock is a multiprocessing RLock: a /dev/shm semaphore the snap's AppArmor
    profile denies. A thread lock leaves it nothing to create."""
    _fake_whisper_stack(monkeypatch, cuda_devices=0)

    asyncio.run(FasterWhisperAdapter("small")._load_model())

    assert [type(lock) for lock in _TQDM_LOCKS] == [type(threading.RLock())]
