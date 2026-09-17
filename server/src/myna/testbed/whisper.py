"""faster-whisper adapter — batch and true streaming modes.

Wraps faster-whisper (CTranslate2 Whisper) behind ``SttService``. Batch mode
(degenerate streaming, I7): decode bounded regions of the pushed audio (see
``myna.testbed.streaming.batch``) and, once the client finishes, emit one
``final`` per Whisper segment, then ``done``.

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
import logging
import os
import re
from collections import deque
from collections.abc import AsyncIterator
from typing import TYPE_CHECKING, Any

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
from myna.testbed.streaming.strategies import Hypothesis, Word

if TYPE_CHECKING:
    import numpy as np
    from numpy.typing import NDArray

_log = logging.getLogger(__name__)

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
# mechanism, and measured 2026-09-02 on corpus/english/manifest-balanced.json it
# was buying nothing: capping it after the second step leaves WER unchanged on
# tiny and base (6.21% and 4.53%, to four decimals) and slightly better on
# small (3.41% -> 3.38%), while cutting p95 decode latency 26% on tiny, 10% on
# base and 25% on small. Dropping it *entirely* measures the same, but the
# second step is kept: it is a recovery path for pathological segments, and no
# fixture here can produce one - the corpus is clean read speech and the
# silence probe is near-silence, so "never helped" is a statement about what
# can be tested, not about what a user will record.
#
# Deliberately not changed with the temperature ladder:
# beam_size stays 5 - dropping it to 1 costs 0.50 pp of WER for ~15%, the same
# trade shape as base int8, which was rejected in T70 - and
# condition_on_previous_text stays True, which costs 0.20 pp for nothing.
_TEMPERATURE_LADDER = (0.0, 0.2)


def batch_decode_options(
    language: str | None, prompt: str | list[int] | None, *, word_timestamps: bool = False
) -> dict[str, object]:
    """Decode parameters for the batch path, in one place.

    Extracted so ``dev/whisper/bench_whisper.py`` and
    ``dev/lab/whisper_decode_sweep.py`` measure the shipped decode rather than
    a copy of it that drifts. Everything not named here is faster-whisper's
    own default.

    ``word_timestamps`` buys a DTW alignment pass: off for dictation, which
    never asks the time, and on whenever the session requests timestamps,
    because whisper's unaligned segment boundaries are quantised to whole
    seconds.
    """
    options: dict[str, object] = {
        "language": _iso639_1(language),
        "initial_prompt": prompt,
        "log_prob_threshold": _LOG_PROB_THRESHOLD,
        "temperature": list(_TEMPERATURE_LADDER),
    }
    if word_timestamps:
        options["word_timestamps"] = True
    return options


def stream_decode_options(
    language: str | None, prompt: str | None, beam_size: int
) -> dict[str, object]:
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


def _timed_segments(
    segment: Any, words: list[tuple[Word, Any]], text: str, offset: float, granularity: str | None
) -> tuple[Segment, ...]:
    """Timestamps for one decoded segment: ``"word"`` yields an entry per word,
    ``"segment"`` one entry spanning the segment, ``None`` nothing.

    ``words`` are the segment's words that survived deduplication, each with
    its faster-whisper alignment (``None`` for a word split out of unaligned
    text); times are absolute. Both timed forms take their boundaries from the
    word alignment rather than ``segment.start``/``end``, which whisper rounds
    to whole seconds. Word granularity degrades to the segment span if the
    alignment produced nothing, so a client that asked for timestamps always
    gets one.
    """
    if granularity is None:
        return ()
    aligned = [(w, a) for w, a in words if a is not None and a.word.strip()]
    if granularity == "word" and aligned:
        return tuple(
            Segment(start=w.start, end=w.end, text=a.word.strip(), score=a.probability)
            for w, a in aligned
        )
    start = aligned[0][0].start if aligned else segment.start + offset
    end = aligned[-1][0].end if aligned else segment.end + offset
    return (Segment(start=start, end=end, text=text, score=segment.avg_logprob),)


# faster-whisper conditions each window on at most this many previous tokens
# (``max_length // 2 - 1``); carrying them across a batch cut keeps that.
_CONTEXT_TOKENS = 223
_UNALIGNED_WORD = re.compile(r"\s*\S+")

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
        self._model: Any | None = None
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

    def _check_compute_type(self) -> None:
        """Refuse arithmetic this device cannot do, before loading weights.

        CTranslate2 does reject an unsupported explicit request - ``float16`` on
        CPU raises - but it raises from inside the model constructor, several
        seconds and one weight load in, with a message that does not say what
        would have been accepted. For a sweep that is a cell that dies on its
        first clip; for a user it is a daemon that starts and then fails on
        first dictation. Ask for the supported set instead, which is a property
        of the build and the device, rather than carrying a table here that goes
        stale on the next CTranslate2 release.

        ``default``/``auto`` are deferrals, not requests, and are left alone -
        ``default`` is how the packaged float16 weights come to be decoded in
        float32 on CPU, which CTranslate2 reports in a startup warning.
        """
        if self._compute_type in ("default", "auto"):
            return
        import ctranslate2

        supported = ctranslate2.get_supported_compute_types(self._device)
        if self._compute_type not in supported:
            raise ValueError(
                f"compute type {self._compute_type!r} is not available on device "
                f"{self._device!r}; this build supports {sorted(supported)}"
            )

    async def _load_model(self) -> Any:
        async with self._model_lock:
            if self._model is None:
                from faster_whisper import WhisperModel

                self._check_compute_type()
                # blocking download + load: keep it off the event loop
                model = await asyncio.to_thread(
                    WhisperModel,
                    self._model_size,
                    device=self._device,
                    compute_type=self._compute_type,
                    download_root=self._download_root,
                )
                self._model = model
                _log.info(
                    "Loaded Whisper model with requested compute type %s; "
                    "effective CTranslate2 compute type is %s",
                    self._compute_type,
                    model.model.compute_type,
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

    async def _load_model_with_heartbeat(self, emit: EventSink) -> Any:
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
            # audio, which would deadlock clients waiting for readiness.
            await emit(TranscriptionProgress(phase=PHASE_READY))

            if self._streaming:
                await self._run_streaming_session(model, config, audio, emit)
                return

            finals = await self._run_batch_session(model, config, audio, emit)
            for final in finals:
                await emit(final)
            await emit(TranscriptionDone(text="".join(f.text for f in finals)))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    async def _run_streaming_session(
        self,
        model: Any,
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

        def decode(samples: NDArray[np.float32], offset: float) -> Hypothesis:
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

    async def _run_batch_session(
        self,
        model: Any,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> list[TranscriptionFinal]:
        """Decode bounded regions, collecting one committed final per Whisper
        segment for presentation after the audio ends.

        Each region continues the previous one the way faster-whisper's own
        30 s windows do: the language detected first is kept and the tokens of
        the text committed so far are the prompt. Word alignment is bought
        only when the client asked for timestamps or a forced cut is involved:
        the region ending at one holds back its last words by their
        timestamps, and the region re-decoding its overlap deduplicates by
        them."""
        import numpy as np

        from myna.testbed.streaming.batch import ends_at_forced_cut, run_deferred_batch
        from myna.testbed.streaming.coverage import (
            RETRY_PADS,
            UNTRANSCRIBED_GAP_S,
            untranscribed_gap,
        )

        granularity = config.timestamp_granularity
        language = config.language
        context: deque[int] = deque(maxlen=_CONTEXT_TOKENS)
        if config.prompt is not None:
            context.extend(_prompt_tokens(model, config.prompt))
        decoded = False
        processed = 0
        region: list[tuple[Any, list[tuple[Word, Any]], float]] = []
        finals: list[TranscriptionFinal] = []

        def decode_once(
            samples: NDArray[np.float32], origin: float, word_timestamps: bool, floor: float
        ) -> list[Word]:
            """One pass, filling ``region``. ``origin`` is the audio time of
            ``samples[0]``, which a nudged re-decode moves back over its pad;
            ``floor`` is the region start, so a word the decoder placed inside
            that pad still times inside the region."""
            nonlocal language
            options = batch_decode_options(
                language,
                list(context) if decoded else config.prompt,
                word_timestamps=word_timestamps,
            )
            segments, info = model.transcribe(samples, **options)
            region.clear()
            words: list[Word] = []

            def at(text: str, start: float, end: float) -> Word:
                return Word(text, max(start + origin, floor), max(end + origin, floor))

            for segment in segments:
                if segment.words:
                    pairs = [(at(a.word, a.start, a.end), a) for a in segment.words]
                else:
                    pairs = [
                        (at(text, segment.start, segment.end), None)
                        for text in _UNALIGNED_WORD.findall(segment.text)
                    ]
                region.append((segment, pairs, origin))
                words.extend(w for w, _ in pairs)
            if language is None:
                language = info.language
            return words

        def decode(samples: NDArray[np.float32], offset: float) -> Hypothesis:
            nonlocal decoded, processed
            first = round(offset * WHISPER_RATE)
            word_timestamps = (
                granularity is not None or first < processed or ends_at_forced_cut(samples)
            )
            words = decode_once(samples, offset, word_timestamps, offset)
            gap = untranscribed_gap(samples, [(w.start - offset, w.end - offset) for w in words])
            if gap >= UNTRANSCRIBED_GAP_S:
                # Whisper skips a sentence in a long region on some inputs, and
                # says nothing about it: the text reads cleanly and the audio it
                # covers is what gives it away (streaming.coverage). Decoding
                # the same audio with its edges nudged recovers the words -
                # measured 2026-09-18, base on the no-gaps stress clip, region
                # 152.5-208.8 s: 137 words with a 3.1 s hole, 146 and 1.8 s at
                # 0.3 s of pad.
                best, best_rank = (words, list(region)), (gap, -len(words))
                for pad_s in RETRY_PADS:
                    pad = np.zeros(round(pad_s * WHISPER_RATE), dtype=samples.dtype)
                    padded = decode_once(
                        np.concatenate([pad, samples, pad]), offset - pad_s, word_timestamps, offset
                    )
                    rank = (
                        untranscribed_gap(
                            samples, [(w.start - offset, w.end - offset) for w in padded]
                        ),
                        -len(padded),
                    )
                    if rank < best_rank:
                        best, best_rank = (padded, list(region)), rank
                    if best_rank[0] < UNTRANSCRIBED_GAP_S:
                        break
                words, region[:] = best[0], best[1]
            decoded = True
            processed = first + len(samples)
            return Hypothesis(words=words)

        async def on_commit(_text: str, committed: list[Word]) -> None:
            kept_ids = {id(w) for w in committed}
            for segment, pairs, offset in region:
                kept = [(w, a) for w, a in pairs if id(w) in kept_ids]
                if len(kept) == len(pairs):
                    text = segment.text.rstrip()
                    tokens = segment.tokens
                else:
                    text = "".join(w.text for w, _ in kept).rstrip()
                    tokens = _text_tokens(model, text) if text else []
                if not text:
                    continue
                context.extend(tokens)
                if not finals:
                    text = text.lstrip()
                # Batch mode is degenerate streaming (I7): committed finals,
                # no segment_index.
                finals.append(
                    TranscriptionFinal(
                        text=text,
                        disposition=Disposition.COMMITTED,
                        segments=_timed_segments(segment, kept, text, offset, granularity),
                    )
                )
            region.clear()

        await run_deferred_batch(audio, emit, decode, on_commit)
        return finals


def _prompt_tokens(model: Any, prompt: str) -> list[int]:
    """The prompt as faster-whisper tokenises an ``initial_prompt`` string."""
    return _text_tokens(model, " " + prompt.strip())


def _text_tokens(model: Any, text: str) -> list[int]:
    ids: list[int] = model.hf_tokenizer.encode(text, add_special_tokens=False).ids
    return ids
