"""GigaAM-v3 e2e RNN-T adapter (feature: Russian dictation).

Batch-mode RNN-T recognition via ONNX Runtime. SberDevices' GigaAM-v3
``e2e`` variants emit punctuated, normalized Russian text directly — the
adapter's raison d'être is best-in-class offline Russian (GigaAM's
evaluation scored e2e ~70:30 over Whisper-large-v3 in side-by-side judged
comparisons) at dictation-friendly CPU cost (~7x realtime, ~430 MB fp32).

The runtime is ONNX Runtime only — no torch. Model artifacts
(encoder/decoder/joint graphs + a SentencePiece piece table) are staged at
component-build time by ``dev/fetch_gigaam_model.py``, which exports them
from the upstream ``.ckpt`` release. Offline by contract (constitution V):
the adapter never downloads at session time.

Audio longer than one encoder pass is split at low-energy points into
~24 s chunks; each chunk emits one committed final (mirrors the whisper
adapter's per-segment finals, I2 concatenation).

Requires the ``gigaam`` extra: ``uv sync --extra gigaam``.
"""

from __future__ import annotations

import asyncio
import gc
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

GIGAAM_RATE = 16_000
GIGAAM_FORMAT = AudioFormat(sample_rate_hz=GIGAAM_RATE, channels=1, sample_width_bytes=2)

# Upstream transcribe() is documented for audio "up to 25 seconds"; the
# encoder itself handles more, but staying under the trained window keeps
# accuracy on long single utterances. Chunks are cut at the quietest point
# near the boundary so words are not split mid-syllable.
_MAX_CHUNK_SECONDS = 24.0
_SILENCE_SEARCH_SECONDS = 2.0
_MIN_TAIL_SECONDS = 0.5

# Greedy RNN-T decoding bounds (mirrors upstream gigaam/onnx_utils.py).
_MAX_LETTERS_PER_FRAME = 3

# Model layout produced by dev/fetch_gigaam_model.py.
_ENCODER_FILE = "encoder.onnx"
_DECODER_FILE = "decoder.onnx"
_JOINT_FILE = "joint.onnx"
_TOKENS_FILE = "tokens.txt"

_LOAD_HEARTBEAT_SECONDS = 2.0
_PROGRESS_INTERVAL_SECONDS = 1.0

# Prediction-network geometry for GigaAM-v3 (1-layer LSTM, hidden 320,
# encoder output 768, vocab 1024 pieces + blank). Pinned per model
# revision; revisit when a second GigaAM line lands.
_PRED_HIDDEN = 320
_PRED_LAYERS = 1
_ENC_DIM = 768


def _default_model_dir() -> str | None:
    """Staged model component (snap) or a local export, like sherpa/funasr.

    Search order: the snap component dir (relative to this file's repo
    layout), then ``~/.cache/myna/gigaam-v3-e2e-rnnt`` for unpackaged runs.
    """
    here = Path(__file__).resolve()
    for base in (
        here.parents[3] / "gigaam-snap" / "components" / "model-gigaam-onnx",
        Path.home() / ".cache" / "myna" / "gigaam-v3-e2e-rnnt",
    ):
        if (base / _TOKENS_FILE).is_file():
            return str(base)
    return None


# ── torchaudio-parity log-mel front-end (no torch at runtime) ───────────────
#
# The upstream encoder graph expects 64-bin log-mel features
# (n_fft 320 / hop 160 / win 320, HTK mel with Hz-domain triangles,
# center=False, power=2, then ln(clamp(x, 1e-9, 1e9))) — torch.stft does not
# export to ONNX, so the features are computed here. Pinned against a
# Python-torchaudio golden reference in tests/test_gigaam_mel.py.

_N_FFT = 320
_HOP = 160
_N_MELS = 64
_SAMPLE_RATE = 16_000


def _hann_periodic(n: int) -> np.ndarray:
    return 0.5 * (1.0 - np.cos(2.0 * np.pi * np.arange(n) / n))


def _htk_mel_filterbank(n_mels: int = _N_MELS) -> np.ndarray:
    """[n_freqs, n_mels] triangular bank, torchaudio conventions (norm=None)."""
    n_freqs = _N_FFT // 2 + 1

    def hz_to_mel(hz: float) -> float:
        return 2595.0 * np.log10(1.0 + hz / 700.0)

    def mel_to_hz(mel: float) -> float:
        return 700.0 * (10.0 ** (mel / 2595.0) - 1.0)

    m_pts = np.linspace(hz_to_mel(0.0), hz_to_mel(_SAMPLE_RATE / 2), n_mels + 2)
    f_pts = mel_to_hz(m_pts)
    fdiff = np.diff(f_pts)
    freqs = np.arange(n_freqs) * (_SAMPLE_RATE / _N_FFT)
    # ramps: rising edge from f_pts[j], falling edge into f_pts[j+2]
    lower = (freqs[:, None] - f_pts[None, :-2]) / fdiff[None, :-1]
    upper = (f_pts[None, 2:] - freqs[:, None]) / fdiff[None, 1:]
    return np.maximum(0.0, np.minimum(lower, upper))


_WINDOW = _hann_periodic(_N_FFT)
_FB = _htk_mel_filterbank()


def log_mel(samples: np.ndarray) -> np.ndarray:
    """float32 [-1, 1] mono samples -> [n_mels, n_frames] log-mel features."""
    if samples.size < _N_FFT:
        return np.zeros((_N_MELS, 0), dtype=np.float32)
    n_frames = 1 + (samples.size - _N_FFT) // _HOP
    frames = np.lib.stride_tricks.as_strided(
        samples,
        shape=(n_frames, _N_FFT),
        strides=(samples.strides[0] * _HOP, samples.strides[0]),
    )
    spec = np.abs(np.fft.rfft(frames * _WINDOW, n=_N_FFT)) ** 2
    mel = spec @ _FB
    return np.log(np.clip(mel, 1e-9, 1e9)).T.astype(np.float32)


def _split_on_quiet(samples: np.ndarray) -> list[np.ndarray]:
    """Split into <= _MAX_CHUNK_SECONDS pieces at low-energy points."""
    limit = int(_MAX_CHUNK_SECONDS * GIGAAM_RATE)
    if samples.size <= limit:
        return [samples]
    step = int(0.4 * GIGAAM_RATE)
    win = max(step // 10, 1)
    out: list[np.ndarray] = []
    start = 0
    while start < samples.size:
        end = min(start + limit, samples.size)
        if end == samples.size:
            # Final piece: hand it over whole (a short tail below the cap is
            # fine; a hair over the cap only marginally stretches the window).
            out.append(samples[start:])
            break
        window = samples[start:end]
        search = window[-int(_SILENCE_SEARCH_SECONDS * GIGAAM_RATE) :]
        rms = np.sqrt(np.convolve(search**2, np.ones(win) / win, mode="same"))
        cut = window.size - search.size + int(np.argmin(rms))
        if step < cut < window.size - int(_MIN_TAIL_SECONDS * GIGAAM_RATE):
            out.append(window[:cut])
            start += cut
        else:
            out.append(window)
            start = end
    return out


class GigaAMRuntime:
    """ONNX sessions + greedy RNN-T decode. Blocking; call from a thread."""

    def __init__(self, model_dir: str) -> None:
        import onnxruntime as ort

        base = Path(model_dir)
        opts = ort.SessionOptions()
        self.enc = ort.InferenceSession(str(base / _ENCODER_FILE), opts, ["CPUExecutionProvider"])
        self.dec = ort.InferenceSession(str(base / _DECODER_FILE), opts, ["CPUExecutionProvider"])
        self.jnt = ort.InferenceSession(str(base / _JOINT_FILE), opts, ["CPUExecutionProvider"])
        self.tokens = (base / _TOKENS_FILE).read_text(encoding="utf-8").splitlines()
        self.blank = len(self.tokens)

    def transcribe(self, samples: np.ndarray) -> str:
        feats = log_mel(samples.astype(np.float64))
        if feats.shape[1] == 0:
            return ""
        enc, enc_len = self.enc.run(
            ["encoded", "encoded_len"],
            {"audio_signal": feats[None, :, :], "length": np.array([feats.shape[1]], dtype=np.int64)},
        )
        t_len = int(enc_len[0])
        hyp: list[int] = []
        labels = np.array([[self.blank]], dtype=np.int64)
        h = np.zeros((_PRED_LAYERS, 1, _PRED_HIDDEN), dtype=np.float32)
        c = np.zeros((_PRED_LAYERS, 1, _PRED_HIDDEN), dtype=np.float32)
        for t in range(min(t_len, enc.shape[2])):
            for _ in range(_MAX_LETTERS_PER_FRAME):
                dec, ho, co = self.dec.run(["dec", "ho", "co"], {"x": labels, "hi": h, "ci": c})
                joint = self.jnt.run(
                    ["joint"],
                    {
                        "enc": enc[0, :, t].reshape(1, _ENC_DIM, 1),
                        "dec": dec.swapaxes(1, 2),  # [1,1,H] -> [1,H,1]
                    },
                )[0]
                k = int(np.argmax(joint))
                if k == self.blank:
                    break
                hyp.append(k)
                labels = np.array([[k]], dtype=np.int64)
                h, c = ho, co
        return _pieces_to_text(hyp, self.tokens)


def _pieces_to_text(ids: list[int], tokens: list[str]) -> str:
    """SentencePiece pieces -> text ('▁' marks word boundaries)."""
    out = "".join(tokens[i] for i in ids if 0 <= i < len(tokens))
    return out.replace("\u2581", " ").strip()


class GigaAMAdapter:
    def __init__(self, model: str | None = None) -> None:
        self._model_dir = model or _default_model_dir()
        self._runtime: GigaAMRuntime | None = None
        self._model_lock = asyncio.Lock()

    @property
    def streaming(self) -> bool:
        return False

    @property
    def candidate(self) -> Candidate:
        label = "v3-e2e-rnnt" if self._model_dir is None else Path(self._model_dir).name
        return Candidate(
            model=f"gigaam-{label}",
            engine="gigaam-rnnt-onnx-cpu",
            streaming_strategy="commit-on-finalize",
        )

    def capabilities(self) -> Capabilities:
        return Capabilities(
            models=(self.candidate.model,),
            languages=("ru",),
            input_formats=(GIGAAM_FORMAT,),
            # The e2e variants emit punctuation + inverse text normalization.
            punctuation=True,
            translation=False,
        )

    async def _load_model(self):
        """Load the ONNX runtime (canonical adapter hook; --preload and
        idle-reload call this by name)."""
        async with self._model_lock:
            if self._runtime is None:
                if not self._model_dir:
                    raise RuntimeError(
                        "no GigaAM model dir: pass --model or stage one with "
                        "dev/fetch_gigaam_model.py"
                    )
                self._runtime = await asyncio.to_thread(GigaAMRuntime, self._model_dir)
        return self._runtime

    async def unload(self) -> None:
        """Idle-unload (T27): drop the sessions; next session reloads."""
        async with self._model_lock:
            self._runtime = None
        gc.collect()

    async def _load_with_heartbeat(self, emit: EventSink):
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
        if fmt.channels != 1 or fmt.sample_width_bytes != 2 or fmt.sample_rate_hz != GIGAAM_RATE:
            await emit(
                TranscriptionError(
                    code="unsupported_audio_format",
                    message=f"need {GIGAAM_RATE} Hz mono S16LE, got "
                    f"{fmt.sample_rate_hz} Hz {fmt.channels}ch "
                    f"{8 * fmt.sample_width_bytes}-bit",
                )
            )
            return

        try:
            runtime = await self._load_with_heartbeat(emit)
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

            samples = np.frombuffer(bytes(buffered), dtype=np.int16).astype(np.float32) / 32768.0
            pieces = await asyncio.to_thread(self._transcribe_all, runtime, samples)

            finals: list[str] = []
            for text in pieces:
                text = text.strip()
                if not text:
                    continue
                finals.append(text)
                await emit(
                    TranscriptionFinal(
                        text=text,
                        disposition=Disposition.COMMITTED,
                    )
                )
            await emit(TranscriptionDone(text=" ".join(finals)))
        except Exception as exc:
            await emit(
                TranscriptionError(code="inference_failed", message=f"{type(exc).__name__}: {exc}")
            )

    @staticmethod
    def _transcribe_all(runtime: GigaAMRuntime, samples: np.ndarray) -> list[str]:
        """Blocking decode; runs in a worker thread."""
        return [runtime.transcribe(chunk) for chunk in _split_on_quiet(samples)]
