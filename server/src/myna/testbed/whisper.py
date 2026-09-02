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
from myna.testbed.harness import StreamingTelemetry

WHISPER_RATE = 16_000
WHISPER_FORMAT = AudioFormat(sample_rate_hz=WHISPER_RATE, channels=1, sample_width_bytes=2)


def _iso639_1(language: str | None) -> str | None:
    """faster-whisper wants a bare ISO 639-1 code ("en"); the corpus uses
    BCP-47-ish tags with region subtags ("en-GB"), which it rejects. Drop the
    region. Keeps this model-specific quirk inside the adapter (house rule)."""
    return language.split("-")[0] if language else None


# Whisper decodes near-silence into training-data boilerplate — our shipped
# whisper-tiny weights return "You" for pure digital silence, and a dictation
# is near-silence whenever the hotkey is tapped twice or a single short word
# lands in a 30 s window. faster-whisper's silence gate is
#
#     should_skip = no_speech_prob > no_speech_threshold
#     if avg_logprob > log_prob_threshold:
#         should_skip = False        # "logprob high enough, keep it anyway"
#
# and the default -1.0 is loose enough that a confidently-decoded "You"
# un-skips the segment. Raising it to -0.5 makes the skip stick.
#
# Measured 2026-08-26 (dev/lab/whisper_silence_probe.py, 12 empty-reference
# inputs: silence, -70 dBFS dither, -45 dBFS room tone, -50 dBFS hum, at
# 1/5/30 s): 5/12 -> 0/12 on tiny and 6/12 -> 0/12 on base, with WER unchanged
# on the balanced tier (6.21%) and on speech padded with 3 s of silence.
#
# Note this is the *opposite* sign to the whisper.cpp constant it was derived
# from (whisper.cpp feeds it to a temperature-fallback ladder, not a silence
# gate, so porting it by value doubles the failure here). `vad_filter=True`
# also reaches 0/12 but costs accuracy on base and strips pause-heavy speech,
# so it is deliberately not used here.
_LOG_PROB_THRESHOLD = -0.5

# Streaming defaults, module-level so ``myna.server.cli`` can reference them
# instead of repeating the literals (it used to, and a default changed in one
# place would silently not change in the other).
#
# STREAM_CADENCE_S is 2.0, not the 1.0 this shipped with. Whisper's encoder
# costs the same per call whatever the window holds (a fixed 30 s of padded
# mel), so streaming cost is ticks x a constant and the cadence is the only
# lever on it. Measured 2026-09-02 over 302 s of speech, 3 runs each: 1.0 ->
# 2.0 takes the encoder duty cycle 45.4% -> 18.2% for an unchanged WER (5.60%
# median both) and 0.57 s more before the first text appears. 3.0 saves only
# 1.5x more and costs another second of that, so 2.0 is the knee.
STREAM_CADENCE_S = 2.0
STREAM_WINDOW_CAP_S = 30.0  # I6, the uncommitted-buffer bound
STREAM_BEAM_SIZE = 1  # greedy re-decode ticks


# Temperature-fallback ladder. faster-whisper's default is six steps
# (0.0 through 1.0): when a decode trips the compression-ratio or
# log-probability rejection test, the segment is decoded again at each higher
# temperature in turn, so one bad segment can cost six decodes. It is a tail
# mechanism, and measured 2026-09-02 on corpus/real/manifest-balanced.json it
# was buying nothing: capping it after the second step leaves WER unchanged on
# tiny and base (6.21% and 4.53%, to four decimals) and slightly better on
# small (3.41% -> 3.38%), while cutting p95 decode latency 26% on tiny, 10% on
# base and 25% on small. Dropping it *entirely* measures the same, but the
# second step is kept: it is a recovery path for pathological segments, and no
# fixture here can produce one - the corpus is clean read speech and the
# silence probe is near-silence, so "never helped" is a statement about what
# can be tested, not about what a user will record.
#
# Deliberately NOT changed at the same time (docs/project-plan.md T82):
# beam_size stays 5 - dropping it to 1 costs 0.50 pp of WER for ~15%, the same
# trade shape as base int8, which was rejected in T70 - and
# condition_on_previous_text stays True, which costs 0.20 pp for nothing.
_TEMPERATURE_LADDER = (0.0, 0.2)


def batch_decode_options(language: str | None, prompt: str | None) -> dict:
    """Decode parameters for the batch path, in one place.

    Extracted so ``dev/whisper/bench_whisper.py`` and
    ``dev/lab/whisper_decode_sweep.py`` measure the shipped decode rather than
    a copy of it that drifts. Everything not named here is faster-whisper's
    own default.
    """
    return {
        "language": _iso639_1(language),
        "initial_prompt": prompt,
        "log_prob_threshold": _LOG_PROB_THRESHOLD,
        "temperature": list(_TEMPERATURE_LADDER),
    }


def stream_decode_options(language: str | None, prompt: str | None, beam_size: int) -> dict:
    """Decode parameters for a streaming re-decode tick. Same rationale as
    [`batch_decode_options`]; the differences from batch are the greedy beam
    and ``word_timestamps``, which the local-agreement strategy needs.

    The temperature ladder is capped here for the batch reason and one more:
    a tick's cost lands directly in the encoder duty cycle, so a segment that
    escalates through six temperatures stalls the whole live display."""
    return {
        "language": _iso639_1(language),
        "initial_prompt": prompt,
        "beam_size": beam_size,
        "word_timestamps": True,
        "vad_filter": False,
        "log_prob_threshold": _LOG_PROB_THRESHOLD,
        "temperature": list(_TEMPERATURE_LADDER),
    }


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
        stream_cadence_s: float = STREAM_CADENCE_S,
        stream_window_cap_s: float = STREAM_WINDOW_CAP_S,
        stream_beam_size: int = STREAM_BEAM_SIZE,  # 5 ≈ batch quality, 1 ≈ 5× cheaper
        stream_telemetry: StreamingTelemetry | None = None,
    ) -> None:
        self._model_size = model_size
        self._device = device
        self._compute_type = compute_type
        self._download_root = download_root
        self._streaming = streaming
        self._stream_cadence_s = stream_cadence_s
        self._stream_window_cap_s = stream_window_cap_s
        self._stream_beam_size = stream_beam_size
        # perf T03: None on every production call path (dev tooling only). The
        # streaming duty cycle is invisible on the wire, so this is the only
        # way to measure it - see StreamingTelemetry's docstring.
        self._stream_telemetry = stream_telemetry
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
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != WHISPER_RATE:
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

            segments = await asyncio.to_thread(self._transcribe, model, bytes(buffered), config)

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
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
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

        options = stream_decode_options(config.language, config.prompt, self._stream_beam_size)

        def decode(samples, offset: float) -> Hypothesis:
            segments, _info = model.transcribe(samples, **options)
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
            telemetry=self._stream_telemetry,
        )
        await emit(TranscriptionDone(text=transcript))

    def _transcribe(self, model, pcm: bytes, config: SessionConfig) -> list:
        """Blocking decode; runs in a worker thread. Audio is already
        ``WHISPER_FORMAT`` (validated in ``run_session``) — no conversion."""
        import numpy as np

        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

        segments, _info = model.transcribe(
            samples, **batch_decode_options(config.language, config.prompt)
        )
        return list(segments)  # drain the generator while still in the thread
