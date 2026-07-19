"""faster-whisper adapter, commit-on-finalize mode (Phase 2, T07).

Wraps faster-whisper (CTranslate2 Whisper) behind ``SttService``. This is the
simplest honest strategy for an AED model: buffer the pushed audio, decode
once when the client finishes, emit one ``final`` per Whisper segment, then
``done``. ``progress`` events (no text) are emitted while audio is being
received so clients can animate an activity indicator.

Streaming via chunked re-decode (LocalAgreement) is T08 and will be a
separate strategy on top of this adapter.

Requires the ``whisper`` extra: ``uv sync --extra whisper``. ``model_size``
is either a bare size name (``"small"``) fetched from Hugging Face on first
use (``Systran/faster-whisper-*``, cached under ``HF_HOME``) or a path to a
local CTranslate2 model directory — the latter is how the snap loads weights
shipped as model components, with no network access. Pass ``download_root``
to pin a lab cache; verify offline runs with ``HF_HUB_OFFLINE=1``.
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator

from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    AudioFormat,
    Capabilities,
    EventSink,
    PcmChunk,
    Segment,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate

WHISPER_RATE = 16_000
WHISPER_FORMAT = AudioFormat(sample_rate_hz=WHISPER_RATE, channels=1, sample_width_bytes=2)


def _iso639_1(language: str | None) -> str | None:
    """faster-whisper wants a bare ISO 639-1 code ("en"); the corpus uses
    BCP-47-ish tags with region subtags ("en-GB"), which it rejects. Drop the
    region. Keeps this model-specific quirk inside the adapter (house rule)."""
    return language.split("-")[0] if language else None
_PROGRESS_INTERVAL_SECONDS = 1.0
# Heartbeat cadence while the model loads. A cold load is a few seconds from
# disk but can be minutes on first use (weight download), during which there
# is no audio to pace progress off — so tick on a timer instead. Coarser than
# the audio cadence to avoid flooding a long download with events.
_LOAD_HEARTBEAT_SECONDS = 2.0


class FasterWhisperAdapter:
    def __init__(
        self,
        model_size: str = "tiny",
        *,
        # "cpu" by default: ctranslate2's "auto" picks CUDA whenever a driver
        # is visible and then hard-fails if the CUDA runtime libs are absent.
        # GPU is an explicit engine choice, mirroring the inference snaps.
        device: str = "cpu",
        compute_type: str = "default",
        download_root: str | None = None,
    ) -> None:
        self._model_size = model_size
        self._device = device
        self._compute_type = compute_type
        self._download_root = download_root
        self._model = None
        self._model_lock = asyncio.Lock()

    @property
    def candidate(self) -> Candidate:
        # ``model_size`` may be a bare size ("small") or a path to a local
        # CTranslate2 model directory (snap model component). Label the
        # candidate by the leaf name either way so result records stay
        # readable instead of carrying an absolute component path.
        label = os.path.basename(self._model_size.rstrip("/")) or self._model_size
        return Candidate(
            model=f"whisper-{label}",
            engine=f"faster-whisper-{self._device}",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        label = os.path.basename(self._model_size.rstrip("/")) or self._model_size
        # ``*.en`` checkpoints are English-only; the rest are multilingual.
        english_only = label.endswith(".en") or label.endswith("-en")
        return Capabilities(
            models=(f"whisper-{label}",),
            languages=("en",) if english_only else ("*",),
            input_formats=(WHISPER_FORMAT,),
            punctuation=True,  # Whisper emits punctuation + capitalisation
            # Whisper can translate→English, but this adapter doesn't wire
            # output_language to the translate task yet — advertise honestly.
            translation=False,
        )

    async def _load_model(self):
        async with self._model_lock:
            if self._model is None:
                from faster_whisper import WhisperModel

                # blocking download + load: keep it off the event loop
                self._model = await asyncio.to_thread(
                    WhisperModel,
                    self._model_size,
                    device=self._device,
                    compute_type=self._compute_type,
                    download_root=self._download_root,
                )
        return self._model

    async def unload(self) -> None:
        """Release the model (idle-unload, T27). Dropping the CTranslate2
        reference frees its CPU/GPU memory; ``_load_model`` reloads on the next
        session. Idempotent."""
        import gc

        async with self._model_lock:
            self._model = None
        gc.collect()

    async def _load_model_with_heartbeat(self, emit: EventSink):
        """Load the model, emitting a ``preparing`` heartbeat throughout so the
        client shows "loading model…" during a slow cold load rather than a
        silent gap. Emits at least once even when the model is already warm."""
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))  # "loading…"
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        return await load

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        # The accepted format is advertised via capabilities() (T24); the
        # client delivers it (audio-push: client owns capture + conversion).
        # We reject mismatches rather than resample — symmetric across rate,
        # channels and width, and no silent low-quality conversion here.
        if (
            fmt.channels != 1
            or fmt.sample_width_bytes != 2
            or fmt.sample_rate_hz != WHISPER_RATE
        ):
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {WHISPER_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            model = await self._load_model_with_heartbeat(emit)
            # Model resident, accept-gate may open: signal `ready` BEFORE pulling
            # audio. The client gates on this (IE115 STATUS{ready}) — without it
            # the client drops all audio waiting for readiness while we wait for
            # audio, a deadlock (see docs/architecture/ie115-lifecycle.md §3A).
            await emit(TranscriptionProgress(phase=PHASE_READY))

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

            segments = await asyncio.to_thread(
                self._transcribe, model, bytes(buffered), config
            )

            want_timestamps = config.timestamp_granularity is not None
            finals: list[str] = []
            for segment in segments:
                text = segment.text.strip()
                if not text:
                    continue
                finals.append(text)
                await emit(
                    TranscriptionFinal(
                        text=text,
                        segments=(
                            (
                                Segment(
                                    start=segment.start,
                                    end=segment.end,
                                    text=text,
                                    score=segment.avg_logprob,
                                ),
                            )
                            if want_timestamps
                            else ()
                        ),
                    )
                )
            await emit(TranscriptionDone(text=" ".join(finals)))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed", message=f"{type(exc).__name__}: {exc}"
                )
            )

    def _transcribe(self, model, pcm: bytes, config: SessionConfig) -> list:
        """Blocking decode; runs in a worker thread. Audio is already
        ``WHISPER_FORMAT`` (validated in ``run_session``) — no conversion."""
        import numpy as np

        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

        segments, _info = model.transcribe(
            samples,
            language=_iso639_1(config.language),
            initial_prompt=config.prompt,
        )
        return list(segments)  # drain the generator while still in the thread
