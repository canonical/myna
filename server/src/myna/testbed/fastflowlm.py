"""FastFlowLM Whisper adapter for AMD XDNA2 NPUs through HRX.

FastFlowLM owns the NPU model implementation and exposes its standalone ASR
path as an OpenAI-compatible HTTP endpoint.  This adapter keeps that private
implementation detail behind Myna's ordinary pushed-PCM session contract.
The inference snap starts the pinned ``flm`` process on loopback; no audio is
read from a device and neither process needs external network access at runtime.
"""

from __future__ import annotations

import asyncio
import io
import json
import re
import time
import urllib.error
import urllib.request
import uuid
import wave
from collections.abc import AsyncIterator

import numpy as np
from numpy.typing import NDArray

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
from myna.testbed.streaming.strategies import UNSPACED_CHARS

FASTFLOWLM_RATE = 16_000
FASTFLOWLM_FORMAT = AudioFormat(
    sample_rate_hz=FASTFLOWLM_RATE,
    channels=1,
    sample_width_bytes=2,
)

_MODEL = "whisper-v3:turbo"
_LOAD_HEARTBEAT_SECONDS = 2.0
_READY_TIMEOUT_SECONDS = 300.0
_REQUEST_TIMEOUT_SECONDS = 300.0

_UNSPACED_RE = re.compile(f"[{UNSPACED_CHARS}]")
_TOKEN_RE = re.compile(f"\\s*(?:[{UNSPACED_CHARS}]|[^\\s{UNSPACED_CHARS}]+)")


class FastFlowLMAdapter:
    """Batch Whisper Large-v3-Turbo served by FastFlowLM on an AMD NPU."""

    def __init__(self, endpoint: str = "http://127.0.0.1:52625") -> None:
        self.endpoint = endpoint.rstrip("/")

    @property
    def candidate(self) -> Candidate:
        return Candidate(
            model=_MODEL,
            engine="fastflowlm-hrx-npu",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=(_MODEL,),
            languages=("*",),
            input_formats=(FASTFLOWLM_FORMAT,),
            punctuation=True,
            translation=False,
            streaming=False,
            runtime={
                "inference_engine": "FastFlowLM",
                "npu_runtime": "HRX",
                "device": "npu",
            },
        )

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None:
        fmt = config.audio_format
        if fmt != FASTFLOWLM_FORMAT:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=(
                        f"need {FASTFLOWLM_RATE} Hz mono S16LE, got "
                        f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                        f"{8 * fmt.sample_width_bytes}-bit"
                    ),
                )
            )
            return
        if config.output_language is not None:
            await emit(
                TranscriptionError(
                    code="translation_not_supported",
                    message="FastFlowLM Whisper serves transcription, not translation",
                )
            )
            return

        try:
            await self._wait_ready_with_heartbeat(emit)
            text = await self._run_batch_session(audio, emit)
            if text:
                await emit(TranscriptionFinal(text=text, disposition=Disposition.COMMITTED))
            await emit(TranscriptionDone(text=text))
        except Exception as exc:
            await emit(
                TranscriptionError(
                    code="inference_failed",
                    message=f"{type(exc).__name__}: {exc}",
                )
            )

    async def _wait_ready_with_heartbeat(self, emit: EventSink) -> None:
        ready = asyncio.ensure_future(asyncio.to_thread(self._wait_ready))
        await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        while not ready.done():
            done, _ = await asyncio.wait({ready}, timeout=_LOAD_HEARTBEAT_SECONDS)
            if not done:
                await emit(TranscriptionProgress(phase=PHASE_PREPARING))
        await ready
        await emit(TranscriptionProgress(phase=PHASE_READY))

    def _wait_ready(self) -> None:
        """Wait until FastFlowLM's HTTP server is accepting requests.

        ``flm serve --asr 1`` loads Whisper before it starts serving, so this
        readiness edge is also Myna's model-residency accept gate.
        """
        deadline = time.monotonic() + _READY_TIMEOUT_SECONDS
        last_error: Exception | None = None
        request = urllib.request.Request(f"{self.endpoint}/v1/models", method="GET")
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(request, timeout=2.0) as response:
                    if 200 <= getattr(response, "status", 200) < 300:
                        return
            except (OSError, urllib.error.URLError) as exc:
                last_error = exc
            time.sleep(0.25)
        suffix = f": {last_error}" if last_error is not None else ""
        raise TimeoutError(f"FastFlowLM did not become ready at {self.endpoint}{suffix}")

    async def _run_batch_session(self, audio: AsyncIterator[PcmChunk], emit: EventSink) -> str:
        """Decode bounded regions once and defer all presentation to finalize."""
        from myna.testbed.streaming.batch import run_deferred_batch
        from myna.testbed.streaming.strategies import Hypothesis, Word

        last_char = ""
        parts: list[str] = []

        def decode(samples: NDArray[np.float32], offset: float) -> Hypothesis:
            nonlocal last_char
            text = self._transcribe_wav(_samples_to_wav(samples)).strip()
            if not text:
                return Hypothesis()
            end = offset + len(samples) / FASTFLOWLM_RATE
            tokens = _TOKEN_RE.findall(text)
            if last_char and not _UNSPACED_RE.match(last_char) and not _UNSPACED_RE.match(text[0]):
                tokens[0] = " " + tokens[0]
            last_char = text[-1]
            return Hypothesis(words=[Word(token, offset, end) for token in tokens])

        async def on_commit(text: str, _words: list[Word]) -> None:
            parts.append(text)

        await run_deferred_batch(audio, emit, decode, on_commit)
        return "".join(parts).strip()

    def _transcribe_wav(self, wav: bytes) -> str:
        boundary = f"myna-{uuid.uuid4().hex}"
        body = _multipart_body(boundary, wav)
        request = urllib.request.Request(
            f"{self.endpoint}/v1/audio/transcriptions",
            data=body,
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=_REQUEST_TIMEOUT_SECONDS) as response:
            payload = json.loads(response.read())
        text = payload.get("text")
        if not isinstance(text, str):
            error = payload.get("error")
            raise RuntimeError(f"FastFlowLM returned no transcript: {error or payload}")
        return text


def _samples_to_wav(samples: NDArray[np.float32]) -> bytes:
    pcm = (np.clip(samples, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
    output = io.BytesIO()
    with wave.open(output, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(FASTFLOWLM_RATE)
        wav.writeframes(pcm)
    return output.getvalue()


def _multipart_body(boundary: str, wav: bytes) -> bytes:
    marker = boundary.encode("ascii")
    return b"".join(
        (
            b"--" + marker + b'\r\nContent-Disposition: form-data; name="model"\r\n\r\n',
            _MODEL.encode(),
            b"\r\n--"
            + marker
            + b'\r\nContent-Disposition: form-data; name="file"; filename="audio.wav"\r\n'
            b"Content-Type: audio/wav\r\n\r\n",
            wav,
            b"\r\n--" + marker + b"--\r\n",
        )
    )
