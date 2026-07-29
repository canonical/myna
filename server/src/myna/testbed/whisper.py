"""faster-whisper adapter — batch and true streaming modes.

Wraps faster-whisper (CTranslate2 Whisper) behind ``SttService``. Batch mode
(degenerate streaming, I7): buffer the pushed audio, decode once when the
client finishes, emit one ``final`` per Whisper segment, then ``done``.

Streaming mode (feature 008): the rolling re-decode loop in
``myna.testbed.streaming`` decodes the uncommitted window on a cadence while
audio is still arriving; the local-agreement strategy decides what to commit
when, and emission rides the 007 committed/unstable dispositions —
append-only commits, display-only unstable hypotheses. (The 008 sweep
compared three strategies; local-agreement was the only SC-001 pass —
see strategies.py.)

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
    Disposition,
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
        streaming: bool = False,  # T020: Enable streaming mode
        stream_cadence_s: float = 1.0,  # seconds of new audio between re-decodes
        stream_window_cap_s: float = 30.0,  # max uncommitted window (I6)
        stream_beam_size: int = 1,  # re-decode beam; 5 ≈ batch quality, 1 ≈ 5× cheaper
    ) -> None:
        self._model_size = model_size
        self._device = device
        self._compute_type = compute_type
        self._download_root = download_root
        self._streaming = streaming
        self._stream_cadence_s = stream_cadence_s
        self._stream_window_cap_s = stream_window_cap_s
        self._stream_beam_size = stream_beam_size
        self._model = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        """Whether this adapter emits progressive committed segments (T027)."""
        return self._streaming

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
            streaming_strategy="local-agreement" if self._streaming else "commit-on-finalize",
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

            if self._streaming:
                await self._run_streaming_session(model, config, audio, emit)
                return

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
                # Natural spacing: segment texts carry leading whitespace;
                # committed finals concatenate verbatim to the transcript
                # (I2). Only the first sheds its leading space.
                text = segment.text.rstrip()
                if not text:
                    continue
                if not finals:
                    text = text.lstrip()
                finals.append(text)
                # Batch mode is degenerate streaming (I7): committed finals,
                # no segment_index.
                await emit(
                    TranscriptionFinal(
                        text=text,
                        disposition=Disposition.COMMITTED,
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
            await emit(TranscriptionDone(text="".join(finals)))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed", message=f"{type(exc).__name__}: {exc}"
                )
            )

    async def _run_streaming_session(
        self,
        model,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        """Feature 008: rolling re-decode with a commit strategy. Emits
        committed/unstable finals while audio is still arriving (FR-001/002);
        end-of-audio resolves the tail (I5) and the loop returns exactly the
        concatenation of committed text (I2) for the terminal done."""
        from myna.testbed.streaming.loop import run_streaming_loop
        from myna.testbed.streaming.strategies import Hypothesis, LocalAgreement, Word

        language = _iso639_1(config.language)
        prompt = config.prompt

        def decode(samples, offset: float) -> Hypothesis:
            segments, _info = model.transcribe(
                samples,
                language=language,
                initial_prompt=prompt,
                beam_size=self._stream_beam_size,
                word_timestamps=True,
                vad_filter=False,
            )
            words: list[Word] = []
            for seg in segments:  # drain the generator (we're in a thread)
                for w in seg.words or []:
                    words.append(Word(text=w.word, start=w.start + offset, end=w.end + offset))
            return Hypothesis(words=words)

        transcript = await run_streaming_loop(
            audio,
            emit,
            decode,
            LocalAgreement(),
            cadence_seconds=self._stream_cadence_s,
            window_cap_seconds=self._stream_window_cap_s,
        )
        await emit(TranscriptionDone(text=transcript))

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
