"""NVIDIA Nemotron / FastConformer adapter via NeMo (T09).

A natively streaming transducer (cache-aware FastConformer-RNNT) behind
``SttService`` — the architectural counterpoint to the AED whisper adapter:
each frame is processed once, and the latency/accuracy tradeoff is a built-in
dial (``att_context_size``) rather than chunk-size tuning.

Batch mode is **commit-on-finalize** (buffer, decode once on finish).
``streaming=True`` (T019) runs the native cache-aware path instead: live PCM
pushes step the transducer once per ~0.5 s of audio through NeMo's
``CacheAwareStreamingAudioBuffer`` + ``conformer_stream_step`` (spike S2
pattern, research.md Decision 5), per-tick hypotheses emit as unstable, and
two-tick-stable word prefixes commit. The ``att_context_size`` dial is the
latency/accuracy knob in both modes.

Requires the ``nemotron`` extra: ``uv sync --extra nemotron`` (pulls
``nemo_toolkit[asr]`` + torch — heavy, CUDA). The cache-aware streaming model
``nvidia/stt_en_fastconformer_hybrid_large_streaming_multi`` is **English-only**
and has native punctuation/capitalisation.

``model_name`` is either a Hugging Face id (downloaded on first use) or a path
to a local ``.nemo`` checkpoint — the latter is how the snap loads its model
component offline.

Verified on hardware (2026-06-14): decode and real-speech dictation work;
latency is excellent (native transducer, ~0.03s finalize). WER on synthetic
espeak audio is unreliable (the model is OOD on it) — judge it on real speech.
Streaming verified (2026-08-04, RTX 4080 Laptop): batch-parity WER on a 30 s
realtime stream, 0.059 s finalize, TTFC 4.5 s — watermarks in
``results/streaming-watermarks.json`` (``emission_008_nemotron_native``).
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import re
from collections.abc import AsyncIterator

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

NEMO_RATE = 16_000
NEMO_FORMAT = AudioFormat(sample_rate_hz=NEMO_RATE, channels=1, sample_width_bytes=2)
_PROGRESS_INTERVAL_SECONDS = 1.0
_LOAD_HEARTBEAT_SECONDS = 2.0

# Words at the hypothesis tail whose right context can still mutate; held
# back from commits (nemotron has no word timestamps, so the guard is in
# words, not the whisper loop's seconds).
_TAIL_GUARD_WORDS = 2

DEFAULT_MODEL = "nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"


def _unwrap_transcript(result) -> str:
    """Pull the transcript text out of NeMo's ``transcribe()`` return value,
    which has drifted across versions: hybrid models hand back a ``(best, all)``
    tuple, and items are ``Hypothesis`` objects (``.text``) on newer NeMo, plain
    strings on older. This absorbs that variance so ``run_session`` sees a str."""
    if isinstance(result, tuple):
        result = result[0]
    item = result[0]
    return getattr(item, "text", item)


def _parse_att_context_size(value: str | None) -> list[int] | None:
    """Parse "left,right" (e.g. "70,0") into [left, right]; None for default."""
    if not value:
        return None
    parts = [int(p) for p in value.split(",")]
    if len(parts) != 2:
        raise ValueError(f"att_context_size must be 'left,right', got {value!r}")
    return parts


def _stable_commit_boundary(prev: str, cur: str, start: int) -> int | None:
    """Char index in ``cur`` up to which text may be committed, or None.

    The committable region is the word-aligned common prefix of the last two
    decoder ticks (text the transducer re-emitted identically), minus a
    ``_TAIL_GUARD_WORDS`` holdback — tail words have no right context yet and
    can still mutate (the RNNT text carries no timestamps, so stability is
    measured in ticks, not seconds). ``start`` is the already-committed
    length; the boundary never moves backwards.
    """
    n = min(len(prev), len(cur))
    i = start
    while i < n and prev[i] == cur[i]:
        i += 1
    words = [m.end() for m in re.finditer(r"\S+", cur[start:i])]
    if len(words) <= _TAIL_GUARD_WORDS:
        return None
    return start + words[-_TAIL_GUARD_WORDS - 1]


class _StreamDecoder:
    """NeMo cache-aware streaming state machine (T019, spike S2 pattern).

    Wraps ``CacheAwareStreamingAudioBuffer`` + ``conformer_stream_step`` for a
    single live stream: ``push()`` appends PCM and, once at least
    ``_STREAM_STEP_SECONDS`` of un-stepped audio is pending (or ``final``),
    drains the buffer's ready chunks through the encoder/decoder, threading
    the caches and the greedy hypothesis across steps. Returns the
    **accumulated** transcript text — decoded from the hypothesis's
    ``y_sequence`` because the streaming path never refreshes ``hyp.text``
    mid-stream (it stays the previous partial's text).

    Two live-feed adjustments versus NeMo's offline simulation loop:
      * only **full** chunks on the model's chunk schedule are stepped
        mid-stream; partial tails stay pending until the final flush. A
        sub-chunk step makes the greedy RNNT consume encoder frames before
        their right context (~13 frames) has arrived and permanently drops
        the tokens that depended on it (measured 2026-08-04: 0.5 s ticks
        lost utterance onsets wholesale, WER 0.29 vs batch 0.0 on the 30 s
        stream; full-chunk ticks restore batch parity).
      * the first ``append_audio`` returns ``stream_id=-1`` even though it
        created stream 0; the id is pinned to 0 so later appends extend the
        same stream instead of silently growing the batch.

    Synchronous and blocking (GPU work) — call via ``asyncio.to_thread``.
    """

    def __init__(self, model) -> None:
        import torch
        from nemo.collections.asr.parts.utils.streaming_utils import (
            CacheAwareStreamingAudioBuffer,
        )

        self._torch = torch
        self._model = model
        self._buffer = CacheAwareStreamingAudioBuffer(model)
        (
            self._cache_last_channel,
            self._cache_last_time,
            self._cache_last_channel_len,
        ) = model.encoder.get_initial_cache_state(batch_size=1)
        self._hyps = None
        self._stream_id = -1
        self._step = 0
        sched = model.encoder.streaming_cfg
        self._chunk_size = sched.chunk_size
        self._shift_size = sched.shift_size

    @staticmethod
    def _sched(value, first: bool) -> int:
        """First-chunk vs steady-state schedule entry (lists are [first, rest])."""
        if isinstance(value, list):
            return value[0] if first else value[1]
        return value

    def _full_chunks_pending(self) -> int:
        """How many complete chunk-schedule steps the buffer can yield now."""
        if self._buffer.buffer is None:
            return 0
        idx = self._buffer.buffer_idx
        rem = int(self._buffer.streams_length[0]) - idx
        n = 0
        while rem >= self._sched(self._chunk_size, idx == 0):
            n += 1
            idx += self._sched(self._shift_size, idx == 0)
            rem = int(self._buffer.streams_length[0]) - idx
        return n

    def push(self, pcm: bytes, *, final: bool = False) -> str | None:
        """Append PCM; decode if a full chunk is pending. Returns the
        accumulated text after a tick, or None when no tick ran."""
        if pcm:
            import numpy as np

            samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
            self._buffer.append_audio(samples, self._stream_id)
            self._stream_id = 0
        if not final and self._full_chunks_pending() == 0:
            return None
        return self._drain(final=final)

    def _drain(self, *, final: bool) -> str:
        if self._buffer.buffer is None:
            return ""
        model = self._model
        steps = self._full_chunks_pending() if not final else None
        iterator = iter(self._buffer)
        stepped = 0
        while steps is None or stepped < steps:
            try:
                chunk_audio, chunk_lengths = next(iterator)
            except StopIteration:
                break
            # NB: the buffer's generator advances buffer_idx *on yield* — pull
            # exactly as many chunks as we step (never "peek and break"), or
            # the pulled-but-unstepped chunk's audio is consumed silently and
            # its tokens are lost (2026-08-04: dropped every other chunk).
            stepped += 1
            drop = model.encoder.streaming_cfg.drop_extra_pre_encoded if self._step else 0
            keep = final and self._buffer.is_buffer_empty()
            with self._torch.inference_mode():
                (
                    _pred,
                    _texts,
                    self._cache_last_channel,
                    self._cache_last_time,
                    self._cache_last_channel_len,
                    best,
                ) = model.conformer_stream_step(
                    processed_signal=chunk_audio,
                    processed_signal_length=chunk_lengths,
                    cache_last_channel=self._cache_last_channel,
                    cache_last_time=self._cache_last_time,
                    cache_last_channel_len=self._cache_last_channel_len,
                    keep_all_outputs=keep,
                    previous_hypotheses=self._hyps,
                    drop_extra_pre_encoded=drop,
                )
            self._hyps = best
            self._step += 1
        return self.text()

    def text(self) -> str:
        if not self._hyps:
            return ""
        ids = self._hyps[0].y_sequence
        ids = ids.tolist() if hasattr(ids, "tolist") else list(ids)
        return self._model.tokenizer.ids_to_text([int(i) for i in ids if int(i) >= 0])


class _StreamEmitter:
    """Committed/unstable event policy over the accumulated transcript (T019).

    Maps the decoder's growing full-text hypothesis onto the 007 wire
    dispositions, enforcing the 008 emission invariants
    (contracts/emission-semantics.md):
      * I1/I2: committed emissions are exact, advancing slices of the
        accumulated text; ``transcript`` is their verbatim concatenation
        (only the utterance's first emission sheds its leading space).
      * I3: unstable emissions are the uncommitted remainder, never indexed.
      * I4/I5: a commit clears the outstanding unstable; ``finish`` commits
        the remainder. If the final text diverged inside the committed
        region (rare — commits were two-tick-stable), the concatenation
        stays canonical and the divergent tail is simply not re-committed.
    """

    def __init__(self) -> None:
        self._raw: list[str] = []  # exact slices of the accumulated text
        self._emitted: list[str] = []  # as emitted (first is lstripped)
        self._committed_len = 0
        self._last_text = ""
        self._last_unstable = ""
        self._seg = 0

    @property
    def transcript(self) -> str:
        return "".join(self._emitted)

    def update(self, text: str) -> list:
        """One decode tick; returns the events to emit (may be empty)."""
        events = []
        boundary = _stable_commit_boundary(self._last_text, text, self._committed_len)
        if boundary is not None and boundary > self._committed_len:
            event = self._commit(text[self._committed_len : boundary])
            self._committed_len = boundary
            if event is not None:
                events.append(event)
            self._last_unstable = ""  # I4
        unstable = self._remainder(text)
        if unstable and unstable != self._last_unstable:
            events.append(
                TranscriptionFinal(text=unstable, disposition=Disposition.UNSTABLE)
            )
            self._last_unstable = unstable
        self._last_text = text
        return events

    def finish(self, final_text: str) -> list:
        """End-of-audio: commit the remaining tail (I5); returns events."""
        events = []
        if final_text.startswith("".join(self._raw)):
            remainder = self._remainder(final_text)
        else:
            remainder = ""  # committed region mutated; concatenation wins (I2)
        if remainder:
            event = self._commit(final_text[self._committed_len :])
            if event is not None:
                events.append(event)
        self._last_unstable = ""
        return events

    def _remainder(self, text: str) -> str:
        tail = text[self._committed_len :]
        return tail.lstrip() if not self._raw else tail

    def _commit(self, raw: str) -> TranscriptionFinal | None:
        self._raw.append(raw)
        text = raw.lstrip() if len(self._raw) == 1 else raw
        if not text:
            return None
        self._emitted.append(text)
        event = TranscriptionFinal(
            text=text,
            disposition=Disposition.COMMITTED,
            segment_index=self._seg,
        )
        self._seg += 1
        return event


class NemotronAdapter:
    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        *,
        # NeMo picks CUDA when available; this model is GPU-oriented.
        device: str = "cuda",
        att_context_size: list[int] | None = None,
        streaming: bool = False,  # T023: Enable streaming mode
    ) -> None:
        self._model_name = model_name
        self._device = device
        self._att_context_size = att_context_size
        self._streaming = streaming
        self._model = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        """Whether this adapter emits progressive committed segments (T027)."""
        return self._streaming

    @property
    def _label(self) -> str:
        """Readable model name: leaf path component, sans ``.nemo`` extension
        for local checkpoints (the snap loads ``.../<file>.nemo``)."""
        label = os.path.basename(self._model_name.rstrip("/")) or self._model_name
        if label.endswith(".nemo"):
            label = label[: -len(".nemo")]
        return label

    @property
    def candidate(self) -> Candidate:
        ctx = (
            f"-ctx{self._att_context_size[0]}.{self._att_context_size[1]}"
            if self._att_context_size
            else ""
        )
        return Candidate(
            model=f"{self._label}{ctx}",
            engine=f"nemo-{self._device}",
            streaming_strategy=(
                "native-transducer" if self._streaming else "commit-on-finalize"
            ),
        )

    def capabilities(self) -> Capabilities:
        # This FastConformer checkpoint is English-only with native
        # punctuation/capitalisation; no translation.
        return Capabilities(
            models=(self._label,),
            languages=("en",),
            input_formats=(NEMO_FORMAT,),
            punctuation=True,
            translation=False,
        )

    async def _load_model(self):
        async with self._model_lock:
            if self._model is None:
                self._model = await asyncio.to_thread(self._load_blocking)
        return self._model

    def _load_blocking(self):  # pragma: no cover - real NeMo load; hardware-only
        from nemo.collections.asr.models import ASRModel

        # A local .nemo checkpoint (snap model component) is restored directly;
        # anything else is treated as a Hugging Face id and downloaded.
        if self._model_name.endswith(".nemo") and os.path.exists(self._model_name):
            model = ASRModel.restore_from(
                restore_path=self._model_name, map_location=self._device
            )
        else:
            model = ASRModel.from_pretrained(
                model_name=self._model_name, map_location=self._device
            )
        # Cache-aware streaming models expose the latency/accuracy dial; other
        # models don't, so only set it when asked and supported.
        if self._att_context_size is not None and hasattr(model, "encoder"):
            model.encoder.set_default_att_context_size(self._att_context_size)
        model.eval()
        return model

    async def unload(self) -> None:
        """Release the model (idle-unload, T27). Drops the reference and returns
        cached CUDA blocks to the driver; the process (and CUDA context) stay —
        full release needs socket-activation exit (T28). Idempotent."""
        import gc

        async with self._model_lock:
            self._model = None
        gc.collect()
        with contextlib.suppress(Exception):
            import torch

            torch.cuda.empty_cache()

    async def _load_model_with_heartbeat(self, emit: EventSink):
        """Emit a ``preparing`` heartbeat while the (slow, cold) model loads —
        NeMo/torch import + CUDA init makes this gap especially long."""
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
        # Accepted format is advertised via capabilities() (T24); the client
        # delivers it. Reject mismatches rather than resample (audio-push: the
        # client owns capture + conversion) — symmetric across rate/channels/width.
        if (
            fmt.channels != 1
            or fmt.sample_width_bytes != 2
            or fmt.sample_rate_hz != NEMO_RATE
        ):
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {NEMO_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            model = await self._load_model_with_heartbeat(emit)
            # Signal `ready` before pulling audio so the client's accept-gate
            # opens (IE115 STATUS{ready}); otherwise client-drops-until-ready and
            # adapter-waits-for-audio deadlock (ie115-lifecycle.md §3A).
            await emit(TranscriptionProgress(phase=PHASE_READY))

            if self._streaming:
                await self._run_streaming_session(model, audio, emit)
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

            text = await asyncio.to_thread(
                self._transcribe, model, bytes(buffered)
            )
            text = text.strip()
            if text:
                # one utterance -> one final; per-word timestamps are a
                # streaming concern, so no segments here
                await emit(TranscriptionFinal(text=text))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed", message=f"{type(exc).__name__}: {exc}"
                )
            )

    async def _run_streaming_session(
        self,
        model,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        """T019 (US2): native frame-once streaming. PCM pushes feed the
        cache-aware transducer (``_StreamDecoder``, spike S2 pattern); each
        tick's accumulated text drives committed/unstable emissions
        (``_StreamEmitter``). End-of-audio flushes the encoder tail
        (``keep_all_outputs``) and commits the remainder (I5)."""
        decoder = _StreamDecoder(model)
        emitter = _StreamEmitter()
        seconds_since_progress = 0.0
        async for chunk in audio:
            text = await asyncio.to_thread(decoder.push, chunk.data)
            produced = False
            if text is not None:
                for event in emitter.update(text):
                    await emit(event)
                    produced = True
            seconds_since_progress += chunk.duration_seconds
            if not produced and seconds_since_progress >= _PROGRESS_INTERVAL_SECONDS:
                seconds_since_progress = 0.0
                await emit(TranscriptionProgress())  # liveness on quiet ticks
        final_text = await asyncio.to_thread(decoder.push, b"", final=True)
        for event in emitter.finish(final_text):
            await emit(event)
        await emit(TranscriptionDone(text=emitter.transcript))

    def _transcribe(self, model, pcm: bytes) -> str:
        """Blocking decode; runs in a worker thread. Returns the transcript.
        Audio is already ``NEMO_FORMAT`` (validated in ``run_session``)."""
        import numpy as np

        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

        result = model.transcribe(audio=[samples], batch_size=1, verbose=False)
        return _unwrap_transcript(result)
