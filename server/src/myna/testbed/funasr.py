"""FunASR / SenseVoice-Small adapter (feature 009).

Batch-mode CTC recognition via ONNX Runtime. Multilingual: auto/zh/en/yue/ja/ko
with auto-detection as default. Unpunctuated output (``punctuation: false``,
sherpa-compatible posture — post-processing deferred to a shared feature).
Both wire dialects supported unchanged through the existing codec.

The runtime is the ``funasr-onnx`` PyPI package (MIT, Alibaba DAMO Academy) —
ONNX Runtime + kaldi-native-fbank + sentencepiece, no torch/NeMo. Model
artifacts are staged from ModelScope at component-build time.

Requires the ``funasr`` extra: ``uv sync --extra funasr``.
"""

from __future__ import annotations

import asyncio
import gc
import os
import re
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

FUNASR_RATE = 16_000
FUNASR_FORMAT = AudioFormat(sample_rate_hz=FUNASR_RATE, channels=1, sample_width_bytes=2)

# Language set matches SenseVoice's lid_dict.
# https://github.com/FunAudioLLM/SenseVoice
_LANGUAGES = ("auto", "zh", "en", "yue", "ja", "ko")

# The adapter is batch-only by spec (FR-004). Warm-up uses identical
# parameters to the reference MyVoiceTyping app (research.md Decision 8).
_WARMUP_SECONDS = 6.0
_WARMUP_AMPLITUDE = 50.0
_WARMUP_SEED = 0

_LOAD_HEARTBEAT_SECONDS = 2.0
_PROGRESS_INTERVAL_SECONDS = 1.0

# Tag stripping regex matches the reference app's rich_transcription_postprocess
# (research.md Decision 6): removes <|zh|>, <|HAPPY|>, <|nospeech|>, etc.
_TAG_RE = re.compile(r"<\|.*?\|>")

_BPE_FILE = "chn_jpn_yue_eng_ko_spectok.bpe.model"


def _default_model_dir() -> str:
    """The ModelScope cache snapshot staged by ``dev/fetch_funasr_model.py``.

    Offline by contract (constitution V): the adapter never downloads at
    session time. ``funasr_onnx``'s own auto-download fallback is never
    exercised — it requires modelscope at runtime (not a server dep) and
    raises a bare string on failure, which is the TypeError seen when the
    dir doesn't exist locally. Mirrors sherpa.py's ``_default_model_dir``.
    """
    cache = Path(os.environ.get("MODELSCOPE_CACHE") or (Path.home() / ".cache" / "modelscope"))
    for pattern in (
        "models/*SenseVoiceSmall*/snapshots/*",  # botaruibo--SenseVoiceSmall-onnx
        "models/*/SenseVoiceSmall*/snapshots/*",  # botaruibo/SenseVoiceSmall-onnx
    ):
        for snapshot in sorted(cache.glob(pattern), reverse=True):
            if (snapshot / _BPE_FILE).is_file():
                return str(snapshot)
    raise FileNotFoundError(
        f"SenseVoice-Small ONNX not staged under {cache / 'models'} — "
        f"fetch it with: uv run python dev/fetch_funasr_model.py"
    )


class FunasrAdapter:
    """SenseVoice-Small ONNX behind ``SttService``."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        language: str = "auto",
        textnorm: str = "woitn",
    ) -> None:
        if language not in _LANGUAGES:
            raise ValueError(f"language must be one of {_LANGUAGES}, got {language!r}")
        if textnorm not in ("woitn", "withitn"):
            raise ValueError(f"textnorm must be 'woitn' or 'withitn', got {textnorm!r}")
        self._model_dir = model_dir
        self._language = language
        self._textnorm = textnorm
        self._model = None
        self._model_lock = asyncio.Lock()

    # ------------------------------------------------------------------
    # Adapter protocol
    # ------------------------------------------------------------------

    @property
    def streaming(self) -> bool:
        return False  # batch-only (FR-004)

    @property
    def candidate(self) -> Candidate:
        label = (
            os.path.basename(self._model_dir.rstrip("/")) if self._model_dir else "sensevoice-small"
        )
        return Candidate(
            model=label,
            engine="funasr-onnx-cpu",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=(self.candidate.model,),
            languages=_LANGUAGES,
            input_formats=(FUNASR_FORMAT,),
            punctuation=False,  # FR-008: unpunctuated
            translation=False,
        )

    # ------------------------------------------------------------------
    # Model lifecycle (T27 idle-unload compatible)
    # ------------------------------------------------------------------

    async def _load_model(self):
        async with self._model_lock:
            if self._model is not None:
                return self._model
            from funasr_onnx import SenseVoiceSmall

            # Always an existing local dir — an explicit --model path, or
            # the pre-staged cache snapshot. Never a repo id (see above).
            model_dir = self._model_dir or _default_model_dir()
            if not Path(model_dir).is_dir():
                raise FileNotFoundError(f"SenseVoice model dir not found: {model_dir}")

            # Auto-detect quantization (FR-016): prefer the int8 export.
            quantize = (Path(model_dir) / "model_quant.onnx").is_file()
            self._model = await asyncio.to_thread(
                SenseVoiceSmall,
                model_dir=model_dir,
                device_id="-1",
                quantize=quantize,
                # 0 is ORT's "you size the pool", which also lets it pin.
                # Not omitted: funasr_onnx's own default is 4, so leaving
                # this out caps us at 4 threads and silently loses pinning.
                intra_op_num_threads=0,
            )
        return self._model

    async def unload(self) -> None:
        """Release the ORT session (T27 idle-unload). Idempotent."""
        async with self._model_lock:
            self._model = None
        gc.collect()

    # ------------------------------------------------------------------
    # Warm-up (FR-009)
    # ------------------------------------------------------------------

    async def _warm_up(self) -> None:
        """Run one inference with synthetic noise so ORT graph-optimization
        costs are paid during ``preparing``, not the first real utterance.

        Parameters match the reference MyVoiceTyping app exactly
        (research.md Decision 8): 6 s low-amplitude Gaussian noise, seed=0.
        """
        rng = np.random.default_rng(_WARMUP_SEED)
        synth = (
            rng.standard_normal(int(FUNASR_RATE * _WARMUP_SECONDS)) * _WARMUP_AMPLITUDE
        ).astype(np.float32)
        await asyncio.to_thread(self._model, synth)
        # SenseVoice output includes noise/empty tags; discard.

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
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != FUNASR_RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=(
                        f"need {FUNASR_RATE} Hz mono S16LE, got "
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
                # Convert s16le → float32 ndarray, same path as sherpa/qwen.
                samples = (
                    np.frombuffer(bytes(buffered), dtype=np.int16).astype(np.float32) / 32768.0
                )
                text = await asyncio.to_thread(self._decode, samples)

            # Tag stripping (FR-005, SC-006): regex before any wire event.
            stripped = _strip_tags(text)
            # Silence decodes to control tags alone, leaving nothing to commit.
            # An empty final is not harmless: the harness counts it as a
            # committed segment and dates time_to_first_final from it. Same
            # guard as the whisper/qwen/nemotron adapters.
            if stripped:
                await emit(TranscriptionFinal(text=stripped, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=stripped))
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

    def _decode(self, samples: np.ndarray) -> str:
        """Run SenseVoiceSmall(waveform) with language + textnorm. Returns
        raw model output (control tags intact — stripped by caller)."""
        result = self._model(
            samples,
            language=self._language,
            textnorm=self._textnorm,
        )
        # SenseVoiceSmall returns list[str]; take first element.
        if isinstance(result, list):
            return result[0] if result else ""
        return str(result)


# ------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------


def _strip_tags(text: str) -> str:
    """Remove SenseVoice rich-transcription control tags."""
    if not text:
        return ""
    return _TAG_RE.sub("", text).strip()
