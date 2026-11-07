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
import functools
import logging
import os
import re
import threading
from collections import deque
from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from myna.core import (
    PHASE_PREPARING,
    PHASE_READY,
    WARNING_GPU_UNAVAILABLE,
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
from myna.server.lifecycle import GPU_UNAVAILABLE_MESSAGE
from myna.testbed.adapter import Candidate, runtime_versions
from myna.testbed.keyword_bias import KeywordCorrector, keyword_corrector, near_misses
from myna.testbed.streaming.strategies import Hypothesis, Word

if TYPE_CHECKING:
    import numpy as np
    from numpy.typing import NDArray

_log = logging.getLogger(__name__)

WHISPER_RATE = 16_000
WHISPER_FORMAT = AudioFormat(sample_rate_hz=WHISPER_RATE, channels=1, sample_width_bytes=2)


# BCP-47 primary subtags Whisper knows under another code.
_WHISPER_CODE = {"nb": "no", "jv": "jw", "fil": "tl"}


def _iso639_1(language: str | None) -> str | None:
    """faster-whisper wants a bare ISO 639-1 code ("en"); the corpus uses
    BCP-47-ish tags with region subtags ("en-GB"), which it rejects. Drop the
    region and use Whisper's own code where it differs. Keeps this
    model-specific quirk inside the adapter (house rule)."""
    if not language:
        return None
    primary = language.split("-")[0]
    return _WHISPER_CODE.get(primary, primary)


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
# Deliberately not changed with the temperature ladder: beam_size stays 5 -
# dropping it to 1 costs 0.50 pp of WER for ~15%, the same trade shape as
# base int8, which was rejected in T70. condition_on_previous_text is no
# longer one value: it is the per-model batch policy now (see
# batch_decode_options).
_TEMPERATURE_LADDER = (0.0, 0.2)


def batch_decode_options(
    language: str | None,
    prompt: str | list[int] | None,
    *,
    word_timestamps: bool = False,
    condition_on_previous_text: bool = True,
    vad_filter: bool = False,
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

    ``condition_on_previous_text`` and ``vad_filter`` are the per-model batch
    decode policy: the inference snaps ship it as MODEL_BATCH_* environment
    in each models/*/model.yaml, and the engine scripts map it to the server
    flags. The defaults are the small-model measured choice - dropping the
    conditioning cost 0.20 pp WER on small for nothing, and VAD cost
    accuracy on base (T71).

    The large weights flip both. Measured 2026-10-01 (RTX 4080 Laptop, the
    balanced corpus plus its 5 min long-form clip, batch mode, fp16): with
    conditioning kept, a silence-born repeated phrase is chained into the
    next window's prompt and the decode loops - large-v3-turbo reached
    80.6% WER on the long-form clip (551 inserted words, "One or two of the
    musicians from the hall." x30) and large-v3 571.8% on a single 15 s
    clip. condition_on_previous_text=False removes the fuel (long-form
    1.9%); vad_filter=True removes the trigger and the boilerplate tail the
    bare fix leaves behind ("Thank you."), for pooled 2.26%/2.65% WER
    (turbo/large-v3) against 2.38%/2.99% without it. vad_filter alone is an
    accelerant, not a fix: stripping the silence lengthens the conditioning
    chain and the long-form loop got worse (121.8% turbo, 38.2% large-v3).
    The streaming path never produced a loop and keeps its own options.
    Bench-verified through the packaged snap the same day: turbo batch
    23.4% -> 2.2% pooled and 80.6% -> 1.9% long-form, large-v3 batch
    6.7% -> 2.6% and 5.6% -> 2.0%, small rows unchanged.
    """
    options: dict[str, object] = {
        "language": _iso639_1(language),
        "initial_prompt": prompt,
        "log_prob_threshold": _LOG_PROB_THRESHOLD,
        "temperature": list(_TEMPERATURE_LADDER),
        "condition_on_previous_text": condition_on_previous_text,
        "vad_filter": vad_filter,
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


@dataclass(frozen=True)
class _Aligned:
    """The alignment a keyword correction stands in for the words it
    replaced: what `_timed_segments` reads of a faster-whisper word."""

    word: str
    probability: float = 1.0


def _corrected_pairs(
    pairs: list[tuple[Word, Any]], corrector: KeywordCorrector | None
) -> list[tuple[Word, Any]]:
    """``pairs`` with each correction folded into one word spanning the words
    it replaced. Needs the alignment's word probabilities, so unaligned text
    passes through."""
    if corrector is None or not pairs or any(a is None for _, a in pairs):
        return pairs
    spans = corrector.corrections([w.text for w, _ in pairs], [a.probability for _, a in pairs])
    out: list[tuple[Word, Any]] = []
    done = 0
    for first, end, text in spans:
        out += pairs[done:first]
        out.append((Word(text, pairs[first][0].start, pairs[end - 1][0].end), _Aligned(text)))
        done = end
    return out + pairs[done:]


# A near miss becomes the keyword when the decoder scores the text with it
# within this many nats per keyword letter of the text as decoded. Tuned on
# the frozen English set's tune half: past 1.0 ordinary false hits climb on
# every model (base 5 -> 10, small 2 -> 6, turbo 0 -> 1 of 8294 at 1.5).
WHISPER_MARGIN_PER_LETTER = 1.0
_EDGES = re.compile(r"(\s*\W*?)(\w.*\w|\w)?(\W*)", re.DOTALL)


_RECORDING = threading.local()


def _record_encodes(model: Any) -> None:
    """Wrap ``model.encode`` once so a `_drain` can see which encoder output
    each segment was decoded from: re-encoding it to score keywords cost more
    than the decode on turbo (2.6 s a window on the CPU)."""
    encode = getattr(model, "encode", None)
    if encode is None or getattr(encode, "_myna_records", False):
        return

    def recording(features: Any, *args: Any, **kwargs: Any) -> Any:
        out = encode(features, *args, **kwargs)
        if getattr(_RECORDING, "active", False):
            _RECORDING.last = (features, out)
        return out

    recording._myna_records = True  # type: ignore[attr-defined]
    model.encode = recording


def _text_log_likelihoods(
    model: Any,
    samples: NDArray[np.float32],
    start: float,
    end: float,
    texts: list[str],
    language: str,
    window: tuple[Any, Any] | None = None,
) -> list[float]:
    """The decoder's teacher-forced log-likelihood of each of ``texts`` over
    ``window`` (the padded features and encoder output the segment was
    decoded from), or else the audio of ``samples[start:end]`` (seconds),
    encoded once for all."""
    import numpy as np
    from faster_whisper.tokenizer import Tokenizer

    if window is not None:
        features, encoded = window
        # The window is padded with exact zeros to the encoder's width.
        content = np.flatnonzero(np.abs(np.asarray(features)).sum(axis=0))
        frames = int(content[-1]) + 1 if len(content) else 1
    else:
        a = max(0, round((start - 0.3) * WHISPER_RATE))
        b = min(len(samples), round((end + 0.3) * WHISPER_RATE), a + 30 * WHISPER_RATE)
        features = model.feature_extractor(samples[a:b])
        frames = features.shape[-1]
        width = getattr(model.feature_extractor, "nb_max_frames", 3000)
        features = np.pad(features, ((0, 0), (0, max(0, width - frames))))[:, :width]
        encoded = model.encode(features)
    tokenizer = Tokenizer(
        model.hf_tokenizer, model.model.is_multilingual, task="transcribe", language=language
    )
    out = []
    for text in texts:
        (result,) = model.model.align(
            encoded, tokenizer.sot_sequence, [tokenizer.encode(text)], frames
        )
        out.append(float(np.log(np.maximum(result.text_token_probs, 1e-12)).sum()))
    return out


def _scored_pairs(
    model: Any,
    samples: NDArray[np.float32],
    segment: Any,
    pairs: list[tuple[Word, Any]],
    keywords: tuple[str, ...],
    language: str | None,
    window: tuple[Any, Any] | None = None,
) -> list[tuple[Word, Any]]:
    """``pairs`` with each near miss of a keyword the audio nearly supports
    folded into the keyword (`_audio_spans`)."""
    spans = _audio_spans(model, samples, segment, pairs, keywords, language, window=window)
    return _apply_spans(pairs, spans)


def _apply_spans(
    pairs: list[tuple[Word, Any]], spans: list[tuple[int, int, str]]
) -> list[tuple[Word, Any]]:
    out = list(pairs)
    for first, end, text in sorted(spans, reverse=True):
        word = Word(text, pairs[first][0].start, pairs[end - 1][0].end)
        out[first:end] = [(word, _Aligned(text))]
    return out


def _audio_spans(
    model: Any,
    samples: NDArray[np.float32],
    segment: Any,
    pairs: list[tuple[Word, Any]],
    keywords: tuple[str, ...],
    language: str | None,
    editable: range | None = None,
    window: tuple[Any, Any] | None = None,
) -> list[tuple[int, int, str]]:
    """(first, end, text) for each run of ``pairs`` (inside ``editable``,
    if given) to fold into a keyword: a near miss whose segment text, with
    the keyword in, the decoder scores within `WHISPER_MARGIN_PER_LETTER`
    nats per letter of the text as decoded. The whole segment is scored, so
    the keyword is judged in its context. Measured on base, small and turbo
    (.kb/model-evaluation.md): holdout recall 36.6 -> 65.9%, 45.7 -> 71.3%,
    50.0 -> 69.5% over the text correction alone."""
    if not keywords or not pairs or any(a is None for _, a in pairs):
        return []
    words = [w.text for w, _ in pairs]
    candidates = []
    for first, end, keyword in near_misses(words, keywords):
        if editable is not None and not (editable.start <= first and end <= editable.stop):
            continue
        run = "".join(words[first:end])
        match = _EDGES.fullmatch(run)
        assert match is not None
        written = keyword
        if keyword.islower() and match.group(2) and match.group(2)[0].isupper():
            written = keyword[0].upper() + keyword[1:]
        candidates.append((first, end, match.group(1) + written + match.group(3), keyword))
    if not candidates:
        return []
    texts = ["".join(words).strip()] + [
        ("".join(words[:first]) + text + "".join(words[end:])).strip()
        for first, end, text, _ in candidates
    ]
    scores = _text_log_likelihoods(
        model, samples, segment.start, segment.end, texts, language or "en", window
    )
    found = sorted(
        (
            ((score - scores[0]) / max(1, sum(ch.isalpha() for ch in keyword)), first, end, text)
            for (first, end, text, keyword), score in zip(candidates, scores[1:], strict=True)
        ),
        reverse=True,
    )
    taken = [False] * len(pairs)
    chosen = []
    for margin, first, end, text in found:
        if margin <= -WHISPER_MARGIN_PER_LETTER or any(taken[first:end]):
            continue
        chosen.append((first, end, text))
        taken[first:end] = [True] * (end - first)
    return chosen


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
        # Per-model batch decode policy - see batch_decode_options for why
        # the large weights want False/True here and the small ones True/False.
        batch_condition_on_previous_text: bool = True,
        batch_vad_filter: bool = False,
    ) -> None:
        self._model_size = model_size
        self._device = device
        self._device_lock = threading.Lock()
        self._compute_type = compute_type
        self._download_root = download_root
        self._streaming = streaming
        self._stream_cadence_s = stream_cadence_s
        self._stream_window_cap_s = stream_window_cap_s
        self._stream_beam_size = stream_beam_size
        self._batch_condition_on_previous_text = batch_condition_on_previous_text
        self._batch_vad_filter = batch_vad_filter
        self._model: Any | None = None
        self._model_lock = asyncio.Lock()
        # Where the loaded model serves, which a GPU engine without a usable
        # GPU makes the CPU (see _load_model).
        self._serving_device = device

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
            # Not a bias: any word list in the prompt breaks the decode
            # (.kb/model-evaluation.md). Words are corrected after it.
            keywords=True,
            streaming=self._streaming,
            runtime={
                **runtime_versions(
                    ("ctranslate2", "ctranslate2"), ("faster_whisper", "faster-whisper")
                ),
                "device": self._serving_device,
            },
        )

    @property
    def gpu_unavailable(self) -> bool:
        return self._serving_device != self._device

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
                import ctranslate2
                from faster_whisper import WhisperModel

                device, compute_type = self._device, self._compute_type
                if device == "cuda" and ctranslate2.get_cuda_device_count() == 0:
                    # No CUDA device CTranslate2 can open: no driver, or one
                    # older than its CUDA runtime. Construction would raise and
                    # fail every utterance; serve on the CPU in the type it
                    # picks for the weights, and say so each session.
                    _log.warning("whisper: no usable CUDA device; serving on the CPU")
                    device, compute_type = "cpu", "default"
                else:
                    self._check_compute_type()
                load = functools.partial(
                    WhisperModel, self._model_size, download_root=self._download_root
                )
                # blocking download + load: keep it off the event loop
                try:
                    model = await asyncio.to_thread(load, device=device, compute_type=compute_type)
                    if device == "cuda":
                        await asyncio.to_thread(_first_decode, model)
                except Exception:
                    # A device it counts can still fail to start (#56). A
                    # broken model fails on the CPU again, and raises.
                    if device != "cuda":
                        raise
                    _log.exception("whisper: CUDA failed to start; serving on the CPU")
                    device, compute_type = "cpu", "default"
                    model = await asyncio.to_thread(load, device=device, compute_type=compute_type)
                self._model = model
                self._serving_device = device
                _log.info(
                    "Loaded Whisper model with requested compute type %s; "
                    "effective CTranslate2 compute type is %s",
                    compute_type,
                    model.model.compute_type,
                )
        return self._model

    def _drain(
        self,
        model: Any,
        samples: Any,
        options: dict[str, Any],
        windows: dict[int, tuple[Any, Any]] | None = None,
    ) -> tuple[list[Any], Any]:
        """Decode, reading every segment. A GPU that started can still fail a
        later decode (out of memory): this one and every later one run on the
        CPU. One that fails there too is not the GPU's, and raises.

        ``windows``, if given, is filled with each segment's padded features
        and encoder output, by ``id``: faster-whisper yields a window's
        segments after encoding it, so the last encode is theirs."""
        if self._model is not None:
            model = self._model
        on = self._serving_device
        try:
            return self._read(model, samples, options, windows)
        except Exception:
            if on != "cuda":
                raise
            _log.exception("whisper: a decode failed on the GPU; serving on the CPU")
            model = self._serve_on_cpu()
            return self._read(model, samples, options, windows)

    @staticmethod
    def _read(
        model: Any,
        samples: Any,
        options: dict[str, Any],
        windows: dict[int, tuple[Any, Any]] | None,
    ) -> tuple[list[Any], Any]:
        if windows is None:
            segments, info = model.transcribe(samples, **options)
            return list(segments), info
        _record_encodes(model)
        _RECORDING.active, _RECORDING.last = True, None
        try:
            segments, info = model.transcribe(samples, **options)
            out = []
            for segment in segments:
                out.append(segment)
                if _RECORDING.last is not None:
                    windows[id(segment)] = _RECORDING.last
            return out, info
        finally:
            _RECORDING.active, _RECORDING.last = False, None

    def _serve_on_cpu(self) -> Any:
        from faster_whisper import WhisperModel

        with self._device_lock:
            if self._serving_device != "cpu":
                self._model = WhisperModel(
                    self._model_size,
                    device="cpu",
                    compute_type="default",
                    download_root=self._download_root,
                )
                self._serving_device = "cpu"
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
            if self.gpu_unavailable:
                await emit(
                    TranscriptionProgress(
                        phase=PHASE_READY,
                        warning=GPU_UNAVAILABLE_MESSAGE,
                        warning_code=WARNING_GPU_UNAVAILABLE,
                    )
                )
            else:
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
        corrector = keyword_corrector(config.keywords)
        # The live passes stay raw: a correction that differs between passes
        # (a merge, a margin near the line) breaks their agreement and stalls
        # the commits. The words a pass commits are corrected once, against
        # that pass's audio and word probabilities.
        last: dict[str, Any] = {}

        def decode(samples: NDArray[np.float32], offset: float) -> Hypothesis:
            windows: dict[int, tuple[Any, Any]] | None = {} if corrector is not None else None
            segments, info = self._drain(model, samples, options, windows)
            words: list[Word] = []
            regions: list[tuple[Any, list[tuple[Word, Any]]]] = []
            for seg in segments:
                pairs = [
                    (Word(text=w.word, start=w.start + offset, end=w.end + offset), w)
                    for w in seg.words or []
                ]
                regions.append((seg, pairs))
                words.extend(w for w, _ in pairs)
            last.update(
                samples=samples,
                windows=windows or {},
                regions=regions,
                language=getattr(info, "language", None) or _iso639_1(config.language),
            )
            return Hypothesis(words=words)

        def corrected(text: str, words: list[Word]) -> str:
            kept = {id(w) for w in words}
            regions = [
                (seg, pairs, [i for i, (w, _) in enumerate(pairs) if id(w) in kept])
                for seg, pairs in last.get("regions", [])
            ]
            if sum(len(mine) for _, _, mine in regions) != len(words):
                return text  # not all from the last pass: commit them as heard
            out: list[Word] = []
            for seg, pairs, mine in regions:
                if mine:
                    lo, hi = mine[0], mine[-1] + 1
                    spans = _audio_spans(
                        model,
                        last["samples"],
                        seg,
                        pairs,
                        config.keywords,
                        last["language"],
                        editable=range(lo, hi),
                        window=last["windows"].get(id(seg)),
                    )
                    part = _apply_spans(pairs[lo:hi], [(a - lo, b - lo, t) for a, b, t in spans])
                    out.extend(w for w, _ in _corrected_pairs(part, corrector))
            if not out:
                return text
            joined = "".join(w.text for w in out).rstrip()
            return joined if text.startswith(" ") else joined.lstrip()

        committed: list[str] = []

        async def on_commit(text: str, words: list[Word]) -> None:
            text = await asyncio.to_thread(corrected, text, words)
            await emit(
                TranscriptionFinal(
                    text=text, disposition=Disposition.COMMITTED, segment_index=len(committed)
                )
            )
            committed.append(text)

        transcript = await run_streaming_loop(
            audio,
            emit,
            decode,
            LocalAgreement(),
            cadence_seconds=self._stream_cadence_s,
            window_cap_seconds=self._stream_window_cap_s,
            on_commit=on_commit if corrector is not None else None,
        )
        await emit(TranscriptionDone(text="".join(committed) if corrector else transcript))

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
        # Correcting toward keywords reads each word's probability, which
        # only the alignment pass produces.
        corrector = keyword_corrector(config.keywords)
        context: deque[int] = deque(maxlen=_CONTEXT_TOKENS)
        if config.prompt is not None:
            context.extend(_prompt_tokens(model, config.prompt))
        decoded = False
        processed = 0
        region: list[tuple[Any, list[tuple[Word, Any]], float]] = []
        finals: list[TranscriptionFinal] = []

        def decode_once(
            samples: NDArray[np.float32],
            origin: float,
            word_timestamps: bool,
            floor: float,
            ceiling: float,
        ) -> list[Word]:
            """One pass, filling ``region``. ``origin`` is the audio time of
            ``samples[0]``, which a nudged re-decode moves back over its pad;
            ``floor`` and ``ceiling`` are the region's own bounds, so a word
            the decoder placed inside a pad, or timed past the end of what it
            was given, still times inside the region."""
            nonlocal language
            options = batch_decode_options(
                language,
                list(context) if decoded else config.prompt,
                word_timestamps=word_timestamps,
                condition_on_previous_text=self._batch_condition_on_previous_text,
                vad_filter=self._batch_vad_filter,
            )
            windows: dict[int, tuple[Any, Any]] | None = {} if corrector is not None else None
            segments, info = self._drain(model, samples, options, windows)
            region.clear()
            words: list[Word] = []

            def at(text: str, start: float, end: float) -> Word:
                def clamp(t: float) -> float:
                    return min(max(t + origin, floor), ceiling)

                return Word(text, clamp(start), clamp(end))

            for segment in segments:
                if segment.words:
                    pairs = [(at(a.word, a.start, a.end), a) for a in segment.words]
                else:
                    pairs = [
                        (at(text, segment.start, segment.end), None)
                        for text in _UNALIGNED_WORD.findall(segment.text)
                    ]
                if corrector is not None:
                    spoken = language or getattr(info, "language", None)
                    pairs = _scored_pairs(
                        model,
                        samples,
                        segment,
                        pairs,
                        config.keywords,
                        spoken,
                        (windows or {}).get(id(segment)),
                    )
                pairs = _corrected_pairs(pairs, corrector)
                region.append((segment, pairs, origin))
                words.extend(w for w, _ in pairs)
            if language is None:
                language = info.language
            return words

        # Words aligned only for keyword confidences are judged as segments
        # are without keywords: onset-only would read one long word ("1717.")
        # as a hole and re-decode the region twice for nothing.
        trust_words = False

        def accounted(offset: float) -> list[tuple[float, float]]:
            """What the last decode accounts for, in region seconds: an
            aligned word only its onset, a segment-level time its whole span
            (see streaming.coverage)."""
            spans: list[tuple[float, float]] = []
            for _segment, pairs, _origin in region:
                if pairs and (pairs[0][1] is None or not trust_words):
                    spans.append((pairs[0][0].start - offset, pairs[-1][0].end - offset))
                else:
                    spans.extend((w.start - offset, w.start - offset) for w, _ in pairs)
            return spans

        def decode(samples: NDArray[np.float32], offset: float) -> Hypothesis:
            nonlocal decoded, processed, trust_words
            first = round(offset * WHISPER_RATE)
            trust_words = (
                granularity is not None or first < processed or ends_at_forced_cut(samples)
            )
            word_timestamps = trust_words or corrector is not None
            end = offset + len(samples) / WHISPER_RATE
            words = decode_once(samples, offset, word_timestamps, offset, end)
            gap = untranscribed_gap(samples, accounted(offset))
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
                        np.concatenate([pad, samples, pad]),
                        offset - pad_s,
                        word_timestamps,
                        offset,
                        end,
                    )
                    rank = (untranscribed_gap(samples, accounted(offset)), -len(padded))
                    if rank < best_rank:
                        best, best_rank = (padded, list(region)), rank
                    if best_rank[0] < UNTRANSCRIBED_GAP_S:
                        break
                if best_rank[0] >= UNTRANSCRIBED_GAP_S:
                    _log.warning(
                        "whisper left %.1f s of the %.1f s region at %.1f s untranscribed "
                        "after %d nudged re-decodes",
                        best_rank[0],
                        len(samples) / WHISPER_RATE,
                        offset,
                        len(RETRY_PADS),
                    )
                words, region[:] = best[0], best[1]
            decoded = True
            processed = first + len(samples)
            return Hypothesis(words=words)

        async def on_commit(_text: str, committed: list[Word]) -> None:
            kept_ids = {id(w) for w in committed}
            for segment, pairs, offset in region:
                kept = [(w, a) for w, a in pairs if id(w) in kept_ids]
                corrected = any(isinstance(a, _Aligned) for _, a in pairs)
                if len(kept) == len(pairs) and not corrected:
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


def _first_decode(model: Any) -> None:
    """Decode a second of silence. CTranslate2 creates its cuBLAS handle at the
    first GEMM, so a GPU cuBLAS cannot run on loads fine and fails here."""
    import numpy as np

    segments, _info = model.transcribe(
        np.zeros(WHISPER_RATE, dtype=np.float32), language="en", beam_size=1, max_new_tokens=1
    )
    for _segment in segments:
        pass


def _prompt_tokens(model: Any, prompt: str) -> list[int]:
    """The prompt as faster-whisper tokenises an ``initial_prompt`` string."""
    return _text_tokens(model, " " + prompt.strip())


def _text_tokens(model: Any, text: str) -> list[int]:
    ids: list[int] = model.hf_tokenizer.encode(text, add_special_tokens=False).ids
    return ids
