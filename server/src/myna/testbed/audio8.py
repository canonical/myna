"""Audio8-ASR-0.1B adapter (feature 010).

Batch-mode autoregressive ASR via the publisher's self-contained ONNX cache
engine (``OnnxCacheAsrEngine``). Multilingual recognition (en/zh/fr/de/ja/ko/
yue) via model-side auto-detection — language *selection* is ``auto``-only
(prompt-based pinning is empirically inert, results/spike-audio8-language.md).
Greedy, bounded generation (no streaming — the model has none; research.md
Decision 1).

The runtime is the publisher's ONNX release, staged as *data* alongside the
model bundle by ``dev/fetch_audio8_model.py`` — nothing CC-BY-NC-licensed is
committed to this GPLv3 tree (research.md Decision 2). The adapter importlib-
loads ``asr_onnx_runtime.py`` from the staged dir (``AUDIO8_MODEL_DIR`` env
override → HF cache snapshot), mirroring the qwen adapter's library pattern.

Requires the ``audio8`` extra: ``uv sync --extra audio8``.
"""

from __future__ import annotations

import asyncio
import gc
import importlib.util
import io
import os
import re
import sys
import wave
from collections.abc import AsyncIterator
from pathlib import Path

import numpy as np

from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    AudioFormat,
    Capabilities,
    Disposition,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate

AUDIO8_RATE = 16_000
AUDIO8_FORMAT = AudioFormat(sample_rate_hz=AUDIO8_RATE, channels=1, sample_width_bytes=2)

# Selectable language: ``auto`` ONLY. The model recognizes seven languages
# (en/zh/fr/de/ja/ko/yue) under model-side auto-detection, but prompt-based
# pinning is empirically inert (results/spike-audio8-language.md, tasks.md
# T004) — the publisher's own runtime hardcodes ``del language``. FR-006 is
# amended accordingly: selection is auto-only; recognition scope stays 7-wide.
_LANGUAGES = ("auto",)
_RECOGNIZED_LANGUAGES = ("en", "zh", "fr", "de", "ja", "ko", "yue")

# Warm-up parameters match the funasr adapter exactly (research.md Decision 9):
# 6 s low-amplitude Gaussian noise, seed=0, so one-time ONNX Runtime graph
# optimization/arena allocation is paid during ``preparing``, not the first
# real utterance (FR-010, SC-003).
_WARMUP_SECONDS = 6.0
_WARMUP_AMPLITUDE = 50.0
_WARMUP_SEED = 0

_LOAD_HEARTBEAT_SECONDS = 2.0
_PROGRESS_INTERVAL_SECONDS = 1.0

# Bounded generation (FR-008): the engine's greedy decode self-terminates at
# max_new_tokens. The cache decoder has a fixed 512-token total budget
# (prompt + output); long utterances eat most of it with audio tokens, so the
# per-utterance cap is clamped against the budget in _decode (see
# _clamp_max_new_tokens). 256 covers short/medium dictation utterances; the
# clamp protects the 30 s tail.
_DEFAULT_MAX_NEW_TOKENS = 256

# Prompt text tokens besides the audio tokens (fixed "Please transcribe this
# audio." instruction + control tokens). Generous over-estimate so the clamp
# always leaves headroom rather than racing the exact tokenizer count.
_PROMPT_TEXT_TOKENS = 16

# Conservative silence RMS gate threshold (Decision 7), on float32 samples in
# [-1, 1]. Catches digital silence and a mic noise floor, never quiet speech.
# Spike-tuned by tasks.md T005; pass ``silence_threshold=None`` to disable.
_DEFAULT_SILENCE_THRESHOLD = 1e-4

# Defense-in-depth sweep (Decision 5). The engine's ``normalize_prediction_text``
# already strips special tokens, ``<|text|>``/``<asr_text>`` splits, and a
# leading "language X" prefix; these catch any residual artifacts the upstream
# normalizer might miss (SC-006).
_TAG_RE = re.compile(r"<\|.*?\|>")
_LANGUAGE_PREFIX_RE = re.compile(r"^\s*language\s+[A-Za-z]+\s+", re.IGNORECASE)

_RUNTIME_FILE = "asr_onnx_runtime.py"
_MODULE_NAME = "audio8_onnx_runtime"


def _default_model_dir() -> str:
    """The HF cache snapshot staged by ``dev/fetch_audio8_model.py``.

    Offline by contract (constitution V): the adapter never downloads at
    session time. ``AUDIO8_MODEL_DIR`` (absolute path to the staged bundle)
    overrides; otherwise the HF cache snapshot containing both the runtime
    source and ``model_bundle/metadata.json`` is located. Mirrors
    funasr.py's ``_default_model_dir``.
    """
    override = os.environ.get("AUDIO8_MODEL_DIR")
    if override:
        return override
    cache = Path(os.environ.get("HF_HOME") or (Path.home() / ".cache" / "huggingface"))
    for snapshot in sorted(
        cache.glob("hub/models--Audio8--Audio8-ASR-0.1B-onnx-runtime/snapshots/*"),
        reverse=True,
    ):
        if (snapshot / _RUNTIME_FILE).is_file() and (
            snapshot / "model_bundle" / "metadata.json"
        ).is_file():
            return str(snapshot)
    raise FileNotFoundError(
        f"Audio8 runtime not staged under {cache / 'hub'} — fetch it with:\n"
        "  uv run python dev/fetch_audio8_model.py --accept-license 'CC-BY-NC-4.0'"
    )


def _load_runtime(model_dir: str):
    """importlib-load ``asr_onnx_runtime.py`` from the staged dir.

    The runtime uses an absolute ``from hotword.hotword_trie import ...``, so
    the staged dir must be on ``sys.path`` for the ``hotword`` package to
    resolve. Loading under a fixed module name keeps repeated loads idempotent.
    """
    model_path = Path(model_dir)
    runtime_path = model_path / _RUNTIME_FILE
    if not runtime_path.is_file():
        raise FileNotFoundError(
            f"{_RUNTIME_FILE} not found under {model_dir} — re-run "
            "dev/fetch_audio8_model.py (engine source is staged alongside the bundle)"
        )
    if str(model_path) not in sys.path:
        sys.path.insert(0, str(model_path))
    if _MODULE_NAME in sys.modules:
        return sys.modules[_MODULE_NAME]
    spec = importlib.util.spec_from_file_location(_MODULE_NAME, runtime_path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[_MODULE_NAME] = module
    spec.loader.exec_module(module)
    return module


class Audio8Adapter:
    """Audio8-ASR-0.1B ONNX cache engine behind ``SttService``."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        language: str = "auto",
        cache_precision: str = "int8",
        audio_precision: str = "int8",
        max_new_tokens: int = _DEFAULT_MAX_NEW_TOKENS,
        num_threads: int = 4,
        device: str = "cpu",
        silence_threshold: float | None = _DEFAULT_SILENCE_THRESHOLD,
        punctuation: bool = True,
    ) -> None:
        if language not in _LANGUAGES:
            raise ValueError(
                f"language selection is 'auto' only, got {language!r} — "
                "prompt-based pinning is inert (results/spike-audio8-language.md); "
                f"recognition covers {_RECOGNIZED_LANGUAGES} under auto-detection"
            )
        if cache_precision not in ("int8", "int4", "fp32"):
            raise ValueError(f"cache_precision must be int8/int4/fp32, got {cache_precision!r}")
        if audio_precision not in ("int8", "fp32"):
            raise ValueError(f"audio_precision must be int8/fp32, got {audio_precision!r}")
        if device not in ("cpu", "cuda"):
            raise ValueError(f"device must be 'cpu' or 'cuda', got {device!r}")
        self._model_dir = model_dir
        self._language = language
        self._cache_precision = cache_precision
        self._audio_precision = audio_precision
        self._max_new_tokens = max_new_tokens
        self._num_threads = num_threads
        self._device = device
        self._silence_threshold = silence_threshold
        self._punctuation = punctuation  # spike-confirmed True (T005, FR-007)
        self._engine = None
        self._max_audio_seconds: float | None = None
        # Engine decode-budget internals, read at load for the max_new_tokens
        # clamp (the cache decoder caps prompt+output at max_total_len tokens).
        self._max_total_len: int | None = None
        self._prompt_merge_factor: int | None = None
        self._hop_length: int | None = None
        self._model_lock = asyncio.Lock()

    # ------------------------------------------------------------------
    # Adapter protocol
    # ------------------------------------------------------------------

    @property
    def streaming(self) -> bool:
        return False  # batch-only (FR-004); the model has no streaming mode

    @property
    def candidate(self) -> Candidate:
        return Candidate(
            model="audio8-asr-0.1b",
            engine=f"audio8-onnx-{'cuda' if self._device == 'cuda' else 'cpu'}",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=("audio8-asr-0.1b",),
            languages=_LANGUAGES,
            input_formats=(AUDIO8_FORMAT,),
            punctuation=self._punctuation,
            translation=False,
        )

    # ------------------------------------------------------------------
    # Model lifecycle (idle-unload compatible)
    # ------------------------------------------------------------------

    async def _load_model(self):
        async with self._model_lock:
            if self._engine is not None:
                return self._engine
            model_dir = Path(self._model_dir or _default_model_dir())
            runtime = _load_runtime(str(model_dir))
            if not (model_dir / "model_bundle" / "metadata.json").is_file():
                raise FileNotFoundError(f"Audio8 model bundle not found under: {model_dir}")
            # Provider selection (FR-018/FR-020): GPU engine fails fast when
            # the CUDA provider is absent — never silently falls back to CPU.
            if self._device == "cuda":
                available = runtime.ort.get_available_providers()
                if "CUDAExecutionProvider" not in available:
                    raise RuntimeError(
                        "GPU engine selected but CUDAExecutionProvider is not "
                        f"available (providers: {available}) — refusing to silently "
                        "fall back to CPU (FR-020)"
                    )
            provider = "CUDAExecutionProvider" if self._device == "cuda" else "CPUExecutionProvider"
            engine_cls = runtime.OnnxCacheAsrEngine
            engine = await asyncio.to_thread(
                # The engine's bundle_dir is the model_bundle/ subdir (upstream
                # calls OnnxCacheAsrEngine("model_bundle")); the runtime source
                # and hotword/ live one level up at the snapshot root.
                engine_cls,
                str(model_dir / "model_bundle"),
                provider=provider,
                intra_op_num_threads=self._num_threads,
                cache_precision=self._cache_precision,
                audio_precision=self._audio_precision,
            )
            self._engine = engine
            self._max_audio_seconds = float(getattr(engine, "max_audio_seconds", 30.0))
            # Cache-decoder budget for the output-token clamp (FR-008).
            cache_graph = getattr(engine, "cache_graph", {}) or {}
            self._max_total_len = int(cache_graph.get("max_total_len", 512))
            self._prompt_merge_factor = int(getattr(engine, "prompt_merge_factor", 4))
            self._hop_length = int(
                getattr(getattr(engine, "feature_extractor", None), "hop_length", 160)
            )
        return self._engine

    async def unload(self) -> None:
        """Release the ONNX sessions. Idempotent (sherpa/whisper pattern)."""
        async with self._model_lock:
            self._engine = None
            self._max_audio_seconds = None
        gc.collect()

    # ------------------------------------------------------------------
    # Warm-up (FR-010)
    # ------------------------------------------------------------------

    async def _warm_up(self) -> None:
        """One inference with synthetic noise before ``ready`` (Decision 9)."""
        rng = np.random.default_rng(_WARMUP_SEED)
        synth = (
            rng.standard_normal(int(AUDIO8_RATE * _WARMUP_SECONDS)) * _WARMUP_AMPLITUDE
        ).astype(np.float32)
        wav = _to_wav_bytes((np.clip(synth, -1.0, 1.0) * 32767).astype(np.int16).tobytes())
        await asyncio.to_thread(
            self._engine.transcribe,
            wav,
            language=self._language_code(),
            max_new_tokens=8,  # discard path — smallest bounded decode
            hotwords=None,
        )

    # ------------------------------------------------------------------
    # Session (FR-001: myna.core session contract)
    # ------------------------------------------------------------------

    async def _load_model_with_heartbeat(self, emit: EventSink):
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        model = await load
        await self._warm_up()
        await emit(TranscriptionProgress(phase=PHASE_READY))
        return model

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        # Audio-push invariant (FR-002): reject off-format, never resample.
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != AUDIO8_RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=(
                        f"need {AUDIO8_RATE} Hz mono S16LE, got "
                        f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                        f"{8 * fmt.sample_width_bytes}-bit"
                    ),
                )
            )
            return

        try:
            await self._load_model_with_heartbeat(emit)
            # Ready AFTER warm-up — the client gates on it.

            # Batch mode (FR-004): accumulate all audio, decode once.
            buffered = bytearray()
            seconds_since_progress = 0.0
            async for chunk in audio:
                buffered.extend(chunk.data)
                seconds_since_progress += chunk.duration_seconds
                if seconds_since_progress >= _PROGRESS_INTERVAL_SECONDS:
                    seconds_since_progress = 0.0
                    await emit(TranscriptionProgress())

            text = ""
            if buffered:
                text = await asyncio.to_thread(self._decode, bytes(buffered))

            # Empty final is not harmless (harness counts it as a committed
            # segment) — same guard as whisper/qwen/nemotron/funasr adapters.
            if text:
                await emit(TranscriptionFinal(text=text, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed",
                    message=f"{type(exc).__name__}: {exc}",
                )
            )

    # ------------------------------------------------------------------
    # Decode
    # ------------------------------------------------------------------

    def _language_code(self) -> str | None:
        # Always None: the only selectable language is auto (spike T004).
        return None

    def _decode(self, pcm: bytes) -> str:
        """Greedy batch decode, unbounded via chunk-and-stitch (FR-009
        amended): the model's audio encoder caps at ``max_audio_seconds``
        (~30 s — the ONNX audio tower is fixed-length), so audio beyond that
        is split into per-chunk decodes and the transcripts stitched, exactly
        the unbounded posture of the other adapters (whisper chunks
        internally; funasr/sherpa feed the full buffer)."""
        if self._silence_threshold is not None:
            samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
            rms = float(np.sqrt(np.mean(samples * samples))) if samples.size else 0.0
            if rms < self._silence_threshold:
                return ""  # near-silence → no decode (FR-005/SC-005 hallucination guard)

        max_bytes = int((self._max_audio_seconds or 30.0) * AUDIO8_RATE) * 2
        if len(pcm) <= max_bytes:
            return self._decode_chunk(pcm)
        chunks: list[str] = []
        for start in range(0, len(pcm), max_bytes):
            text = self._decode_chunk(pcm[start : start + max_bytes])
            if text:
                chunks.append(text)
        return " ".join(chunks)

    def _decode_chunk(self, pcm: bytes) -> str:
        """One ≤ max_audio_seconds decode: WAV-wrap, clamp the output-token
        budget (FR-008), sanitize (Decision 5)."""
        result = self._engine.transcribe(
            _to_wav_bytes(pcm),
            language=self._language_code(),
            max_new_tokens=self._clamp_max_new_tokens(len(pcm) // 2),
            hotwords=None,
        )
        return _strip_residual(result.get("text", "") if isinstance(result, dict) else str(result))

    def _clamp_max_new_tokens(self, sample_count: int) -> int:
        """Cap output tokens so prompt + output stay within the cache decoder's
        fixed ``max_total_len`` budget (512). Mirrors the engine's
        ``ark_audio_token_count`` (hop_length, merge_factor); the fixed text
        prompt is over-estimated by ``_PROMPT_TEXT_TOKENS`` so the clamp can
        never overflow. Falls back to the configured cap before load."""
        if self._max_total_len is None:
            return self._max_new_tokens
        mel_frames = sample_count // (self._hop_length or 160)
        downsampled = (mel_frames + 1) // 2
        audio_tokens = max(downsampled // (self._prompt_merge_factor or 4), 1)
        prompt = audio_tokens + _PROMPT_TEXT_TOKENS
        return max(1, min(self._max_new_tokens, self._max_total_len - prompt))


# ------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------


def _to_wav_bytes(pcm: bytes) -> bytes:
    """Wrap raw s16le mono 16 kHz PCM in a WAV container (stdlib, no deps)."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(AUDIO8_RATE)
        wav.writeframes(pcm)
    return buf.getvalue()


def _strip_residual(text: str) -> str:
    """Defense-in-depth output sweep (Decision 5)."""
    if not text:
        return ""
    text = _LANGUAGE_PREFIX_RE.sub("", text)
    text = _TAG_RE.sub("", text)
    return re.sub(r"\s+", " ", text).strip()
