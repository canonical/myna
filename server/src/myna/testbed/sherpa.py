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

Punctuation and capitalisation are restored, not decoded: the transducer's
vocabulary is 1025 tokens whose only punctuation is an apostrophe, so a second
model has to do it (``_punctuate``, and ``dev/fetch_sherpa_punct_model.py`` for
the weights). Where it runs follows the emission mode, because the commit
contract and punctuation want opposite things - see ``_run_push_loop``.

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

# The punctuation + truecasing model (k2-fsa's conversion of Edge-Punct-Casing,
# Apache-2.0). Text in, text out - it never sees audio - so it is a plain
# post-pass on committed text. int8: 7.5 MB, 39 ms to load, 52 MB resident,
# 3.7 ms per utterance (measured 2026-09-08 over corpus/english).
PUNCT_MODEL = "sherpa-onnx-online-punct-en-2024-08-06"
_PUNCT_FILES = ("model.int8.onnx", "bpe.vocab")
# One thread: the model is small enough that a pool costs more than it saves,
# and it runs on the commit path beside the recognizer's own two.
PUNCT_NUM_THREADS = 1

# Zeros appended at end-of-audio so the encoder can run past the last real
# frame, sized from this model's geometry (10 ms fbank frames, 65-frame
# window, 56-frame chunk shift, 480 ms = 48 frames of lookahead).
_TAIL_PAD_FRAMES = 65 + 56 + 48
_TAIL_PAD_SAMPLES = _TAIL_PAD_FRAMES * SHERPA_RATE // 100


def _default_model_dir() -> str:
    """The HF cache snapshot (downloads on first use; HF_HUB_OFFLINE=1 uses the
    cache — dev/fetch_sherpa_model.py stages it)."""
    from huggingface_hub import snapshot_download

    return snapshot_download(HF_REPO_ID)


def _default_punct_dir() -> str | None:
    """The staged punctuation model, or None when it was never fetched.

    None rather than an exception: the punctuation model is not on the Hub (it
    ships in a GitHub release), so nothing downloads it implicitly, and a dev
    checkout that staged only the transducer must still transcribe. What it
    must not do is claim `punctuation: true` and then commit lowercase text -
    hence a resolved-or-None dir that capabilities reads.
    """
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return _punct_dir_if_complete(cache / "myna" / "models" / PUNCT_MODEL)


def _punct_dir_if_complete(path: Path | str | None) -> str | None:
    """``path`` if it holds both punctuation artifacts, else None."""
    if path is None:
        return None
    path = Path(path)
    return str(path) if all((path / f).is_file() for f in _PUNCT_FILES) else None


# Intra-op threads for all three ONNX sessions. Small on purpose, and the one
# adapter-level thread cap besides parakeet's (T65).
#
# sherpa-onnx forwards this straight to intra_op_num_threads, so ORT never
# pins the pool (affinity happens only when ORT sizes it itself) - but pinning
# does not begin to pay for the wrong width here. Measured 2026-09-03 on
# sherpa-onnx 1.13.7 + onnxruntime 1.27.0 over 1020 s of the English corpus,
# 3 timed reps each, as RTF: 1 -> 0.0535, **2 -> 0.0372**, 3 -> 0.0363,
# 4 -> 0.0365, 6 -> 0.0466, 16 -> 0.1834, and 0 (ORT sizes and *does* pin,
# 45 sched_setaffinity calls) -> 0.1342. The streaming transducer decodes
# 480 ms chunks whose tensors are far too small to divide, so three
# machine-wide pools spend their time in thread barriers: pinning buys 1.37x
# against 16, and a small pool buys 4.9x.
#
# 2 rather than the 3 that measured lowest: the floor from 2 to 4 is flat
# inside run-to-run spread, and a constant that cannot walk into the bad
# region on a wider machine is worth more than 2%. It is also sherpa-onnx's
# own default, so the value only has to be passed to keep it from drifting.
DEFAULT_NUM_THREADS = 2


class SherpaAdapter:
    """sherpa-onnx OnlineRecognizer behind ``SttService``."""

    def __init__(
        self,
        model_dir: str | None = None,
        *,
        streaming: bool = False,
        num_threads: int = DEFAULT_NUM_THREADS,
        punct_dir: str | None = None,
        punctuate: bool = True,
    ) -> None:
        """``punct_dir`` overrides where the punctuation model is found;
        ``punctuate=False`` turns restoration off and returns the raw
        transducer output.

        The directory is resolved here, not at load, because
        ``capabilities()`` has to answer before a session opens and must not
        promise punctuation the process cannot deliver. Resolution is two
        ``is_file`` calls.
        """
        self._model_dir = model_dir
        self._streaming = streaming
        self._num_threads = num_threads
        if not punctuate:
            self._punct_dir = None
        elif punct_dir is None:
            self._punct_dir = _default_punct_dir()
        elif (resolved := _punct_dir_if_complete(punct_dir)) is None:
            # An explicit path is an operator asking for punctuation by name.
            # Silently falling back to unpunctuated output there would ship a
            # snap whose transcripts are lowercase for a reason nothing states.
            raise FileNotFoundError(
                f"punctuation model incomplete at {punct_dir}: expected "
                f"{', '.join(_PUNCT_FILES)} - stage it with "
                "dev/fetch_sherpa_punct_model.py, or pass punctuate=False"
            )
        else:
            self._punct_dir = resolved
        self._recognizer = None
        self._punct = None
        self._model_lock = asyncio.Lock()

    @property
    def punctuates(self) -> bool:
        """Whether committed text will carry punctuation and case."""
        return self._punct_dir is not None

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
            # unpunctuated text (verified on the real corpus, 2026-07-29), so
            # this is true of the *service*, not the weights: it holds only
            # while the restoration model is staged.
            punctuation=self.punctuates,
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

    async def _load_punct(self):
        """The punctuation model, loaded once. None when none is staged.

        Held under the same lock as the recognizer and released by the same
        ``unload``: it is 52 MB resident, which is worth reclaiming with the
        rest when a session goes idle.
        """
        if self._punct_dir is None:
            return None
        async with self._model_lock:
            if self._punct is None:
                import sherpa_onnx

                self._punct = await asyncio.to_thread(
                    sherpa_onnx.OnlinePunctuation,
                    sherpa_onnx.OnlinePunctuationConfig(
                        model_config=sherpa_onnx.OnlinePunctuationModelConfig(
                            cnn_bilstm=f"{self._punct_dir}/model.int8.onnx",
                            bpe_vocab=f"{self._punct_dir}/bpe.vocab",
                            num_threads=PUNCT_NUM_THREADS,
                        )
                    ),
                )
        return self._punct

    def _punctuate(self, text: str) -> str:
        """Restore punctuation and casing. A no-op without the model.

        Never fatal: a transcript that reaches the user lowercase is a
        degraded transcript, but one that does not reach them at all because
        the cosmetic pass raised is a lost utterance.
        """
        if self._punct is None or not text:
            return text
        try:
            return self._punct.add_punctuation_with_case(text) or text
        except Exception:  # noqa: BLE001 - see docstring
            return text

    async def unload(self) -> None:
        """Release the recognizer (idle-unload, T27). Idempotent."""
        import gc

        async with self._model_lock:
            self._recognizer = None
            self._punct = None
        gc.collect()

    async def _load_model_with_heartbeat(self, emit: EventSink):
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        recognizer = await load
        # Inside `preparing` too: 39 ms, but on the commit path if left to the
        # first segment, and commits are what the user is waiting on.
        await self._load_punct()
        return recognizer

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

            await self._run_push_loop(recognizer, audio, emit)
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    async def _run_push_loop(
        self,
        recognizer,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        """Native push loop, shared by both modes: partial results → unstable
        (display-only, never restate committed text — sherpa resets its
        segment at each endpoint, so post-endpoint partials only cover new
        audio, I3); endpoint-detected segments → committed with monotonic
        ``segment_index`` (I1); at end-of-audio the outstanding partial
        resolves to committed (I5) and the terminal transcript is the verbatim
        concatenation (I2 — segments after the first carry a synthetic leading
        space, since sherpa strips its results).

        Batch mode is the same loop with the intermediate emissions withheld:
        the segments are accumulated and land as the single committed final
        that I7 asks for.

        **Where punctuation runs differs between the two, and it has to.**
        Punctuation is a sentence-level judgement, but I3/I4 say committed text
        is never restated, so the two modes get the best each can have:

        - streaming: each segment is punctuated as it commits. Cheap (3.7 ms),
          no protocol change, and the cost is real - a segment is punctuated
          without the next one's context, which loses most sentence-final
          marks. Measured on four segments of one utterance: whole text gives
          "He rang again, this time harder, still no answer. What would she do
          about that? The confounded wretch"; per-segment gives "He rang again,
          this time harder" / "Still no answer" / "What would she do about
          that" / "The confounded wretch".
        - batch: nothing is committed until the end, so the whole transcript is
          punctuated in one pass with full context and lands correct.

        The third option - hold segment N until N+1 endpoints, punctuate with
        one segment of lookback, then commit N - buys streaming most of batch's
        quality for one segment of commit latency. Deliberately not taken here;
        it is a latency trade that wants measuring against the dictation
        cadence, not a detail. Left as a follow-up.

        Partials are never punctuated: they are display-only and re-emitted on
        every decode, so punctuating them would spend the pass ~20x more often
        for text that is about to be replaced, and make the case of a word
        flicker as the hypothesis extends.
        """
        stream = recognizer.create_stream()
        committed: list[str] = []
        segment_index = 0
        last_unstable = ""
        seconds_since_progress = 0.0

        async def commit(text: str) -> None:
            nonlocal segment_index, last_unstable
            text = text.strip()
            if not text:
                return
            # Streaming only: in batch the whole transcript is punctuated once
            # below, with the context this call does not have.
            if self._streaming:
                # Off-loop like every other model call here: 3.7 ms is small,
                # but the loop is concurrently draining pushed audio.
                text = await asyncio.to_thread(self._punctuate, text)
            # Verbatim-concat spacing (I2): sherpa strips segment text, so
            # every segment after the first carries a synthetic leading space.
            if committed:
                text = " " + text
            if self._streaming:
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
            elif self._streaming and text and text != last_unstable:
                await emit(TranscriptionFinal(text=text, disposition=Disposition.UNSTABLE))
                last_unstable = text
            seconds_since_progress += chunk.duration_seconds
            if seconds_since_progress >= _PROGRESS_INTERVAL_SECONDS:
                seconds_since_progress = 0.0
                await emit(TranscriptionProgress())  # liveness

        # I5: resolve the tail — flush and commit whatever is outstanding.
        tail = await asyncio.to_thread(self._flush, recognizer, stream)
        await commit(tail)
        transcript = "".join(committed)
        if not self._streaming:
            # I7 degenerate: one committed segment, the whole transcript - and
            # the one place punctuation gets to see the whole thing.
            transcript = await asyncio.to_thread(self._punctuate, transcript)
            await emit(TranscriptionFinal(text=transcript, disposition=Disposition.COMMITTED))
        await emit(TranscriptionDone(text=transcript))

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
        """Drain the audio left over after the last chunk.

        The encoder only ever consumes whole windows, so when the audio stops
        the last words are still unencoded: up to a chunk shift of real frames
        are unprocessed, the step covering them needs a full window present,
        and the transducer needs its lookahead as right context before it
        emits their tokens. ``_TAIL_PAD_SAMPLES`` zeros supply all three;
        ``input_finished`` then flushes the fbank remainder.
        """
        pad = np.zeros(_TAIL_PAD_SAMPLES, dtype=np.float32)
        stream.accept_waveform(SHERPA_RATE, pad)
        stream.input_finished()
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        return recognizer.get_result(stream)
