"""Qwen3-ASR adapter via the pure-C runtime, commit-on-finalize (T10a).

Wraps the zero-dependency C inference engine (``libqwen_asr.so``) behind
``SttService`` using ctypes FFI. This is the *parsimonious* Qwen3 path: the
runtime is OpenBLAS + a ~170 KB shared object — no torch, no CUDA, no Python ML
stack — so it ships as a tiny CPU engine. The architectural counterpoint is the
vLLM/GPU adapter (T10b, ``qwen_vllm.py``); both serve the same family, selected
per hardware by the snap's engines (cpu → this, nvidia-gpu → vLLM).

The C library exposes exactly our audio-push shape::

    qwen_ctx_t *qwen_load(const char *model_dir);
    char *qwen_transcribe_audio(qwen_ctx_t *, const float *samples, int n);  /* malloc'd */
    void qwen_free(qwen_ctx_t *);

so the adapter feeds raw float32 mono 16 kHz samples straight in and gets the
transcript back — no file path, no resampling (audio-push: the client owns
capture + conversion).

**Scope (honest):** commit-on-finalize, like the first whisper/nemotron cuts —
buffer the pushed audio, decode once on finish via ``qwen_transcribe_audio``,
emit one ``final`` then ``done``. The C runtime *has* a streaming mode
(``qwen_transcribe_stream`` with a monotonic commit frontier that matches our
never-retracted ``final``), but it is measurably sub-realtime on commodity CPUs
(verified 2026-06-15), so live streaming is a follow-up, not the MVP.

The library is located via ``QWEN_ASR_LIB`` (absolute path) or the loader
search path (``libqwen_asr.so``); the snap sets the former from its runtime
component. ``model`` is a path to a model directory (safetensors + tokenizer);
the C runtime does no downloading, so the snap ships weights as a component and
points ``model`` at it.
"""

from __future__ import annotations

import array
import asyncio
import ctypes
import os
from collections.abc import AsyncIterator

from myna.core import (
    PHASE_PREPARING,
    AudioFormat,
    Capabilities,
    EventSink,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.adapter import Candidate

QWEN_RATE = 16_000
QWEN_FORMAT = AudioFormat(sample_rate_hz=QWEN_RATE, channels=1, sample_width_bytes=2)
_PROGRESS_INTERVAL_SECONDS = 1.0
_LOAD_HEARTBEAT_SECONDS = 2.0

# The C runtime forces language by full English name ("Italian"), not ISO code,
# and otherwise auto-detects from audio. Map the codes our corpus uses; anything
# unmapped falls through to auto-detection (the runtime's strong default).
_ISO_TO_QWEN_LANG = {
    "en": "English",
    "zh": "Chinese",
    "de": "German",
    "fr": "French",
    "es": "Spanish",
    "pt": "Portuguese",
    "it": "Italian",
    "ja": "Japanese",
    "ko": "Korean",
    "ru": "Russian",
}

# 30 languages per the model card / qwen_supported_languages_csv().
_QWEN_LANGUAGES = (
    "zh", "en", "yue", "ar", "de", "fr", "es", "pt", "id", "it", "ko", "ru",
    "th", "vi", "ja", "tr", "hi", "ms", "nl", "sv", "da", "fi", "pl", "cs",
    "fil", "fa", "el", "ro", "hu", "mk",
)


def _qwen_language(language: str | None) -> str | None:
    """Map an IE114 language tag ("en", "en-GB") to a name the C runtime forces,
    or None to let it auto-detect. Keeps this model quirk inside the adapter."""
    if not language:
        return None
    return _ISO_TO_QWEN_LANG.get(language.split("-")[0].lower())


class _QwenLib:
    """Thin ctypes binding to ``libqwen_asr.so``, loaded once per process.

    Holds the function prototypes; the loaded model context (``qwen_ctx_t *``)
    lives in the adapter so it can be released on ``unload()``.
    """

    def __init__(self, lib_path: str) -> None:
        self._lib = ctypes.CDLL(lib_path)
        self._libc = ctypes.CDLL("libc.so.6")  # for free() of the returned string
        self._libc.free.argtypes = [ctypes.c_void_p]

        lib = self._lib
        lib.qwen_load.restype = ctypes.c_void_p
        lib.qwen_load.argtypes = [ctypes.c_char_p]
        lib.qwen_free.argtypes = [ctypes.c_void_p]
        # Returned char* is malloc'd by the library; type as void_p so ctypes
        # doesn't copy-and-leak, then free() it ourselves.
        lib.qwen_transcribe_audio.restype = ctypes.c_void_p
        lib.qwen_transcribe_audio.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_float),
            ctypes.c_int,
        ]
        lib.qwen_set_force_language.restype = ctypes.c_int
        lib.qwen_set_force_language.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.qwen_set_prompt.restype = ctypes.c_int
        lib.qwen_set_prompt.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    def load(self, model_dir: str) -> int:
        ctx = self._lib.qwen_load(model_dir.encode())
        if not ctx:
            raise RuntimeError(f"qwen_load failed for {model_dir!r}")
        return ctx

    def free(self, ctx: int) -> None:
        self._lib.qwen_free(ctx)

    def set_force_language(self, ctx: int, language: str | None) -> None:
        # NULL clears any forced language (auto-detect). A -1 return means the
        # name is unsupported; fall back to auto rather than failing the session.
        name = language.encode() if language else None
        if self._lib.qwen_set_force_language(ctx, name) != 0 and name is not None:
            self._lib.qwen_set_force_language(ctx, None)

    def set_prompt(self, ctx: int, prompt: str | None) -> None:
        self._lib.qwen_set_prompt(ctx, prompt.encode() if prompt else None)

    def transcribe_audio(self, ctx: int, samples: "array.array") -> str:
        """``samples`` is a stdlib ``array('f')`` of normalised mono float32."""
        buf = (ctypes.c_float * len(samples)).from_buffer(samples)
        ptr = self._lib.qwen_transcribe_audio(ctx, buf, len(samples))
        if not ptr:
            return ""
        try:
            return ctypes.string_at(ptr).decode("utf-8", errors="replace")
        finally:
            self._libc.free(ptr)


def _resolve_lib_path(explicit: str | None) -> str:
    """Resolve the shared library: explicit arg, then ``QWEN_ASR_LIB``, then the
    loader search path (bare ``libqwen_asr.so``)."""
    return explicit or os.environ.get("QWEN_ASR_LIB") or "libqwen_asr.so"


class QwenCAdapter:
    def __init__(
        self,
        model: str,
        *,
        lib_path: str | None = None,
    ) -> None:
        self._model = model
        self._lib_path = _resolve_lib_path(lib_path)
        self._lib: _QwenLib | None = None
        self._ctx: int | None = None
        self._model_lock = asyncio.Lock()

    @property
    def _label(self) -> str:
        return os.path.basename(self._model.rstrip("/")) or self._model

    @property
    def candidate(self) -> Candidate:
        return Candidate(
            model=f"qwen-{self._label}",
            engine="qwen-c-cpu",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        # Multilingual (30 languages) with native punctuation. Translation is
        # reachable via forced language but this adapter doesn't wire
        # output_language to it yet — advertise honestly (mirrors whisper).
        return Capabilities(
            models=(f"qwen-{self._label}",),
            languages=_QWEN_LANGUAGES,
            input_formats=(QWEN_FORMAT,),
            punctuation=True,
            translation=False,
        )

    async def _load_model(self) -> None:
        async with self._model_lock:
            if self._ctx is None:
                self._lib, self._ctx = await asyncio.to_thread(self._load_blocking)

    def _load_blocking(self) -> tuple[_QwenLib, int]:
        lib = _QwenLib(self._lib_path)
        return lib, lib.load(self._model)

    async def unload(self) -> None:
        """Release the model context (idle-unload, T27): free the C-side weights
        and KV buffers, drop the reference. The shared library stays mapped (it's
        tiny); ``_load_model`` reloads weights on the next session. Idempotent."""
        import gc

        async with self._model_lock:
            if self._ctx is not None and self._lib is not None:
                self._lib.free(self._ctx)
            self._ctx = None
        gc.collect()

    async def _load_model_with_heartbeat(self, emit: EventSink) -> None:
        load = asyncio.ensure_future(self._load_model())
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not load.done():
            done, _ = await asyncio.wait({load}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        await load

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        # Accepted format is advertised via capabilities() (T24); the client
        # delivers it. Reject mismatches rather than resample (audio-push).
        if (
            fmt.channels != 1
            or fmt.sample_width_bytes != 2
            or fmt.sample_rate_hz != QWEN_RATE
        ):
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {QWEN_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            await self._load_model_with_heartbeat(emit)

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

            text = await asyncio.to_thread(self._transcribe, bytes(buffered), config)
            text = text.strip()
            if text:
                await emit(TranscriptionFinal(text=text))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed", message=f"{type(exc).__name__}: {exc}"
                )
            )

    def _transcribe(self, pcm: bytes, config: SessionConfig) -> str:
        """Blocking decode; runs in a worker thread. Audio is already
        ``QWEN_FORMAT`` (validated in ``run_session``) — convert int16 → the
        normalised float32 the C entry point expects, using only stdlib so the
        C path carries no pip dependencies (the parsimony point)."""
        assert self._lib is not None and self._ctx is not None  # loaded above
        self._lib.set_force_language(self._ctx, _qwen_language(config.language))
        self._lib.set_prompt(self._ctx, config.prompt)

        ints = array.array("h")  # signed 16-bit, native order (S16LE on amd64)
        ints.frombytes(pcm)
        samples = array.array("f", (s / 32768.0 for s in ints))
        return self._lib.transcribe_audio(self._ctx, samples)
