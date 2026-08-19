"""sherpa-onnx adapter — turnkey native streaming transducer (008 US4).

The "build-vs-adopt" comparison point that concludes the streaming
investigation: sherpa-onnx's ``OnlineRecognizer`` does the push loop, frame
caching, and endpoint detection natively — no custom decode loop to maintain.
Per-step partials → unstable; endpoint-detected segments → committed (007
dispositions, invariants I1–I7 as usual).

Model: a NeMo-family streaming FastConformer transducer exported by k2-fsa
(``csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8``),
staged by ``dev/fetch_sherpa_model.py`` — pre-exported, so no local k2 export
step (research.md Decision 8's Zipformer fallback stays available the same
way). The 80/480/1040 ms variants are sherpa's latency dial (the
``att_context_size`` analog); 480 ms is the default middle point.

Requires the ``sherpa`` extra: ``uv sync --extra sherpa``. sherpa-onnx's
native lib needs onnxruntime 1.27.x's version node (see pyproject); the wheel
doesn't bundle libonnxruntime, so ``sherpa_onnx.libs/libonnxruntime.so`` must
point at the pip package's lib (dev/fetch_sherpa_model.py --fix-libs sets it
up; the snap bundles its own).
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator

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

SHERPA_RATE = 16_000
SHERPA_FORMAT = AudioFormat(sample_rate_hz=SHERPA_RATE, channels=1, sample_width_bytes=2)

HF_REPO_ID = "csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8"
# sherpa's OnlineRecognizer endpoint rules: an utterance-final pause commits
# (rule1), a mid-utterance pause commits (rule2), and rule3 bounds very long
# segments. 1.2 s trailing silence ≈ the dictation pause cadence.
RULE1_TRAILING_SILENCE_S = 2.4
RULE2_TRAILING_SILENCE_S = 1.2
RULE3_MIN_UTTERANCE_S = 20.0

_LOAD_HEARTBEAT_SECONDS = 2.0
_PROGRESS_INTERVAL_SECONDS = 1.0


def _default_model_dir() -> str:
    """The HF cache snapshot (downloads on first use; HF_HUB_OFFLINE=1 uses the
    cache — dev/fetch_sherpa_model.py stages it)."""
    from huggingface_hub import snapshot_download

    return snapshot_download(HF_REPO_ID)


class SherpaAdapter:
    """sherpa-onnx OnlineRecognizer behind ``SttService``."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        streaming: bool = False,
        num_threads: int = 2,
    ) -> None:
        self._model_dir = model_dir
        self._streaming = streaming
        self._num_threads = num_threads
        self._recognizer = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        return self._streaming

    @property
    def candidate(self) -> Candidate:
        label = (
            os.path.basename(self._model_dir.rstrip("/"))
            if self._model_dir
            else "fastconformer-streaming-transducer-480ms-int8"
        )
        return Candidate(
            model=label,
            engine="sherpa-onnx-cpu",
            streaming_strategy="native-transducer" if self._streaming else "commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=(self.candidate.model,),
            languages=("en",),
            input_formats=(SHERPA_FORMAT,),
            # The streaming FastConformer transducer exports lowercase,
            # unpunctuated text (verified on the real corpus, 2026-07-29).
            punctuation=False,
            translation=False,
        )

    async def _load_model(self):
        async with self._model_lock:
            if self._recognizer is None:
                import sherpa_onnx

                model_dir = self._model_dir or await asyncio.to_thread(_default_model_dir)
                self._recognizer = await asyncio.to_thread(
                    sherpa_onnx.OnlineRecognizer.from_transducer,
                    f"{model_dir}/tokens.txt",
                    f"{model_dir}/encoder.int8.onnx",
                    f"{model_dir}/decoder.int8.onnx",
                    f"{model_dir}/joiner.int8.onnx",
                    num_threads=self._num_threads,
                    sample_rate=SHERPA_RATE,
                    feature_dim=80,
                    model_type="nemo_transducer",
                    enable_endpoint_detection=True,
                    rule1_min_trailing_silence=RULE1_TRAILING_SILENCE_S,
                    rule2_min_trailing_silence=RULE2_TRAILING_SILENCE_S,
                    rule3_min_utterance_length=RULE3_MIN_UTTERANCE_S,
                    decoding_method="greedy_search",
                    debug=False,
                )
        return self._recognizer

    async def unload(self) -> None:
        """Release the recognizer (idle-unload, T27). Idempotent."""
        import gc

        async with self._model_lock:
            self._recognizer = None
        gc.collect()

    async def _load_model_with_heartbeat(self, emit: EventSink):
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
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
        # Audio-push invariant: the client owns capture + conversion; we
        # advertise the accepted format and reject mismatches, never resample.
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != SHERPA_RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {SHERPA_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            recognizer = await self._load_model_with_heartbeat(emit)
            # Ready BEFORE pulling audio — the client gates on it
            # (docs/architecture/ie115-lifecycle.md §3A).
            await emit(TranscriptionProgress(phase=PHASE_READY))

            if self._streaming:
                await self._run_streaming_session(recognizer, audio, emit)
                return

            # Batch (I7 degenerate): push everything, finalize, one committed.
            stream = recognizer.create_stream()
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
                samples = (
                    np.frombuffer(bytes(buffered), dtype=np.int16).astype(np.float32) / 32768.0
                )
                text = await asyncio.to_thread(self._decode_oneshot, recognizer, stream, samples)
            await emit(TranscriptionFinal(text=text, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    @staticmethod
    def _decode_oneshot(recognizer, stream, samples: np.ndarray) -> str:
        """Push all audio and return the full transcript, accumulating across
        any endpoint boundaries.

        The recognizer is created with ``enable_endpoint_detection=True``
        (needed by the streaming path); in batch mode an endpoint that fires
        mid-audio (e.g. a noise-induced pause) would cause the ``is_ready``
        loop to exit early and ``get_result`` to return only text up to that
        endpoint, silently dropping the remainder. Looping over endpoints and
        accumulating segments gives the same behaviour as the streaming
        committed-segment concat (I2).
        """
        stream.accept_waveform(SHERPA_RATE, samples)
        stream.input_finished()
        segments: list[str] = []
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
            if recognizer.is_endpoint(stream):
                seg = recognizer.get_result(stream).strip()
                if seg:
                    segments.append(seg)
                recognizer.reset(stream)
        tail = recognizer.get_result(stream).strip()
        if tail:
            segments.append(tail)
        return " ".join(segments)

    async def _run_streaming_session(
        self,
        recognizer,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        """Native push loop: partial results → unstable (display-only, never
        restate committed text — sherpa resets its segment at each endpoint,
        so post-endpoint partials only cover new audio, I3); endpoint-detected
        segments → committed with monotonic ``segment_index`` (I1); at
        end-of-audio the outstanding partial resolves to committed (I5) and
        the terminal transcript is the verbatim concatenation (I2 — segments
        after the first carry a synthetic leading space, since sherpa strips
        its results)."""
        stream = recognizer.create_stream()
        committed: list[str] = []
        segment_index = 0
        last_unstable = ""

        async def commit(text: str) -> None:
            nonlocal segment_index, last_unstable
            text = text.strip()
            if not text:
                return
            # Verbatim-concat spacing (I2): sherpa strips segment text, so
            # every segment after the first carries a synthetic leading space.
            if committed:
                text = " " + text
            await emit(
                TranscriptionFinal(
                    text=text,
                    disposition=Disposition.COMMITTED,
                    segment_index=segment_index,
                )
            )
            committed.append(text)
            segment_index += 1
            last_unstable = ""  # I4: commit clears unstable

        async for chunk in audio:
            samples = np.frombuffer(chunk.data, dtype=np.int16).astype(np.float32) / 32768.0
            endpoint, text = await asyncio.to_thread(self._push, recognizer, stream, samples)
            if endpoint:
                await commit(text)
            elif text and text != last_unstable:
                await emit(TranscriptionFinal(text=text, disposition=Disposition.UNSTABLE))
                last_unstable = text
            elif not text and not endpoint:
                await emit(TranscriptionProgress())  # liveness on quiet ticks

        # I5: resolve the tail — flush and commit whatever is outstanding.
        tail = await asyncio.to_thread(self._flush, recognizer, stream)
        await commit(tail)
        await emit(TranscriptionDone(text="".join(committed)))

    @staticmethod
    def _push(recognizer, stream, samples: np.ndarray) -> tuple[bool, str]:
        """Push one chunk and decode all ready frames. Returns (endpoint, text)
        — on endpoint, text is the segment to commit; otherwise the current
        partial (may be empty)."""
        stream.accept_waveform(SHERPA_RATE, samples)
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        if recognizer.is_endpoint(stream):
            text = recognizer.get_result(stream)
            recognizer.reset(stream)
            return True, text
        return False, recognizer.get_result(stream)

    @staticmethod
    def _flush(recognizer, stream) -> str:
        """Drain any outstanding audio after the last audio chunk.

        After an endpoint + reset mid-clip, the tail of the audio may be
        shorter than one full encoder chunk (480 ms for this model). A brief
        zero-pad gives the encoder the right-context frames it needs to emit
        the final word; ``input_finished`` then flushes the remainder.
        """
        _TAIL_PAD_S = 0.32  # just over one 480 ms chunk's lookahead frames
        pad = np.zeros(int(_TAIL_PAD_S * SHERPA_RATE), dtype=np.float32)
        stream.accept_waveform(SHERPA_RATE, pad)
        stream.input_finished()
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        return recognizer.get_result(stream)
