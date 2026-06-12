"""NVIDIA Nemotron / FastConformer adapter via NeMo (T09).

A natively streaming transducer (cache-aware FastConformer-RNNT) behind
``SttService`` — the architectural counterpoint to the AED whisper adapter:
each frame is processed once, and the latency/accuracy tradeoff is a built-in
dial (``att_context_size``) rather than chunk-size tuning.

This first cut is **commit-on-finalize** (buffer, decode once on finish) so it
drops straight into the existing matrix alongside whisper. True frame-at-a-time
streaming — where this architecture actually pays off — is the follow-up
(converges with T08); the ``att_context_size`` dial is already plumbed.

Requires the ``nemotron`` extra: ``uv sync --extra nemotron`` (pulls
``nemo_toolkit[asr]`` + torch — heavy, CUDA). The cache-aware streaming model
``nvidia/stt_en_fastconformer_hybrid_large_streaming_multi`` is **English-only**
and has native punctuation/capitalisation.

STATUS: UNVERIFIED on hardware — written against the NeMo 2.x API; the
``transcribe``/``set_default_att_context_size`` calls are the expected
iteration points on first run (NeMo's API drifts across versions).
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator

from myna.core import (
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate

NEMO_RATE = 16_000
_PROGRESS_INTERVAL_SECONDS = 1.0
_LOAD_HEARTBEAT_SECONDS = 2.0

DEFAULT_MODEL = "nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"


def _parse_att_context_size(value: str | None) -> list[int] | None:
    """Parse "left,right" (e.g. "70,0") into [left, right]; None for default."""
    if not value:
        return None
    parts = [int(p) for p in value.split(",")]
    if len(parts) != 2:
        raise ValueError(f"att_context_size must be 'left,right', got {value!r}")
    return parts


class NemotronAdapter:
    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        *,
        # NeMo picks CUDA when available; this model is GPU-oriented.
        device: str = "cuda",
        att_context_size: list[int] | None = None,
    ) -> None:
        self._model_name = model_name
        self._device = device
        self._att_context_size = att_context_size
        self._model = None
        self._model_lock = asyncio.Lock()

    @property
    def candidate(self) -> Candidate:
        label = os.path.basename(self._model_name.rstrip("/")) or self._model_name
        ctx = (
            f"-ctx{self._att_context_size[0]}.{self._att_context_size[1]}"
            if self._att_context_size
            else ""
        )
        return Candidate(
            model=f"{label}{ctx}",
            engine=f"nemo-{self._device}",
            streaming_strategy="commit-on-finalize",
        )

    async def _load_model(self):
        async with self._model_lock:
            if self._model is None:
                self._model = await asyncio.to_thread(self._load_blocking)
        return self._model

    def _load_blocking(self):
        from nemo.collections.asr.models import ASRModel

        model = ASRModel.from_pretrained(model_name=self._model_name, map_location=self._device)
        # Cache-aware streaming models expose the latency/accuracy dial; other
        # models don't, so only set it when asked and supported.
        if self._att_context_size is not None and hasattr(model, "encoder"):
            model.encoder.set_default_att_context_size(self._att_context_size)
        model.eval()
        return model

    async def _load_model_with_heartbeat(self, emit: EventSink):
        """Emit a progress heartbeat while the (slow, cold) model loads."""
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress())
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress())
        return await load

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        if fmt.channels != 1 or fmt.sample_width_bytes != 2:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need mono S16LE, got {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            model = await self._load_model_with_heartbeat(emit)

            buffered = bytearray()
            seconds_since_progress = 0.0
            async for chunk in audio:
                buffered.extend(chunk.data)
                seconds_since_progress += chunk.duration_seconds
                if seconds_since_progress >= _PROGRESS_INTERVAL_SECONDS:
                    seconds_since_progress = 0.0
                    await emit(TranscriptionProgress())

            if not buffered:
                await emit(TranscriptionDone(text=""))
                return

            text = await asyncio.to_thread(
                self._transcribe, model, bytes(buffered), fmt.sample_rate_hz
            )
            text = text.strip()
            if text:
                # one utterance -> one final; per-word timestamps are a
                # streaming concern (follow-up), so no segments here
                await emit(TranscriptionFinal(text=text))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed", message=f"{type(exc).__name__}: {exc}"
                )
            )

    def _transcribe(self, model, pcm: bytes, rate: int) -> str:
        """Blocking decode; runs in a worker thread. Returns the transcript."""
        import numpy as np

        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
        if rate != NEMO_RATE:
            n_out = int(len(samples) * NEMO_RATE / rate)
            samples = np.interp(
                np.linspace(0.0, len(samples) - 1, n_out),
                np.arange(len(samples)),
                samples,
            ).astype(np.float32)

        result = model.transcribe(audio=[samples], batch_size=1, verbose=False)
        # Hybrid models can return (best, all); take the best list. Items are
        # Hypothesis objects (.text) on newer NeMo, plain strings on older.
        if isinstance(result, tuple):
            result = result[0]
        item = result[0]
        return getattr(item, "text", item)
