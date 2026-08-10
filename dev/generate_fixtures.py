"""Generate the synthetic fixture corpus (T03, synthetic tier).

    uv run python dev/generate_fixtures.py [--out server/fixtures]

Synthesizes the UD129 accuracy-matrix categories with espeak-ng into 16 kHz
mono S16LE WAV files plus a ``manifest.json`` (see ``myna.testbed.corpus``).
Reference transcripts are exact by construction. Output is CC0: the texts are
project-authored and espeak-ng output carries no recording rights.

Noise-category clips are clean clips mixed with seeded Gaussian noise at a
fixed SNR, so noisy and quiet runs are comparable clip-for-clip.

Requires the espeak-ng *library* (``libespeak-ng1`` + ``espeak-ng-data``
debs); the CLI is not needed. Synthetic speech is fine for plumbing, latency,
and regression baselines, but it is not a substitute for recorded human
speech when judging real-world accuracy — that tier is a separate task (T25).
"""

from __future__ import annotations

import argparse
import ctypes
import json
import math
import random
import sys
import wave
from array import array
from dataclasses import dataclass
from pathlib import Path

TARGET_RATE = 16_000
NOISE_SNR_DB = 10.0
NOISE_SEED = 20260612  # fixed: regeneration must be deterministic

REPO_ROOT = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class ClipSpec:
    id: str
    text: str
    language: str
    category: str
    voice: str


@dataclass(frozen=True)
class NoiseSpec:
    """A noise-category variant of an existing clean clip."""

    id: str
    base_id: str
    snr_db: float = NOISE_SNR_DB


CLIP_SPECS: tuple[ClipSpec, ...] = (
    ClipSpec(
        "quiet-pangram",
        "The quick brown fox jumps over the lazy dog.",
        "en",
        "quiet",
        "en-us",
    ),
    ClipSpec(
        "quiet-weather",
        "It looks like rain later this afternoon, so bring an umbrella when you leave.",
        "en",
        "quiet",
        "en-us",
    ),
    ClipSpec(
        "commands-editing",
        "New paragraph. Delete the last word. Select all. Full stop.",
        "en",
        "commands",
        "en-us",
    ),
    ClipSpec(
        "commands-minimal",
        "Copy that.",
        "en",
        "commands",
        "en-us",
    ),
    ClipSpec(
        "long-form-letter",
        "Thank you for your message about the schedule change. We reviewed the "
        "proposal during the weekly meeting and agreed that the new plan is "
        "workable, provided the documentation is updated before the release. "
        "Please confirm whether the testing team has the equipment they need, "
        "and whether the laboratory will be available in the second week of "
        "July. We will send the revised timeline to everyone by Friday "
        "afternoon at the latest.",
        "en",
        "long-form",
        "en-us",
    ),
    ClipSpec(
        "technical-stack",
        "Restart the PipeWire daemon, then verify that the WebSocket endpoint "
        "over the Unix domain socket streams transcription events as JSON "
        "payloads.",
        "en",
        "technical",
        "en-us",
    ),
    ClipSpec(
        "names-people",
        "Henry Jones met Siobhan O'Connor and Joaquin Marquez outside Edinburgh Waverley station.",
        "en",
        "names",
        "en-us",
    ),
    ClipSpec(
        "acronyms-it",
        "The CPU and GPU usage of the ASR model is exported over SSH to the "
        "NAS, and the JSON logs are parsed by the CLI.",
        "en",
        "acronyms",
        "en-us",
    ),
    ClipSpec(
        "accent-rp",
        "The quick brown fox jumps over the lazy dog.",
        "en-GB",
        "accent",
        "en-gb-x-rp",
    ),
    ClipSpec(
        # NOTE(charles): Longer term there will actually be accented clips.
        # The eSpeak backend certainly *doesn't* have a real Scottish accent ;)
        "accent-scotland",
        "The quick brown fox jumps over the lazy dog.",
        "en-GB",
        "accent",
        "en-gb-scotland",
    ),
    ClipSpec(
        "non-english-german",
        "Der schnelle braune Fuchs springt über den faulen Hund.",
        "de",
        "non-english",
        "de",
    ),
)

NOISE_SPECS: tuple[NoiseSpec, ...] = (
    NoiseSpec("noise-pangram-snr10", "quiet-pangram"),
    NoiseSpec("noise-letter-snr10", "long-form-letter"),
)


class EspeakSynthesizer:
    """Minimal ctypes binding to libespeak-ng for offline WAV synthesis."""

    _AUDIO_OUTPUT_RETRIEVAL = 1
    _POS_CHARACTER = 1
    _CHARS_UTF8 = 1
    _CALLBACK = ctypes.CFUNCTYPE(
        ctypes.c_int, ctypes.POINTER(ctypes.c_short), ctypes.c_int, ctypes.c_void_p
    )

    def __init__(self) -> None:
        try:
            self._lib = ctypes.CDLL("libespeak-ng.so.1")
        except OSError as exc:
            raise RuntimeError(
                "libespeak-ng.so.1 not found — install the 'libespeak-ng1' "
                "and 'espeak-ng-data' packages"
            ) from exc
        self.native_rate = self._lib.espeak_Initialize(self._AUDIO_OUTPUT_RETRIEVAL, 0, None, 0)
        if self.native_rate <= 0:
            raise RuntimeError("espeak_Initialize failed (is espeak-ng-data installed?)")
        self._buffer = bytearray()
        # keep a reference: espeak holds the pointer for the process lifetime
        self._callback = self._CALLBACK(self._collect)
        self._lib.espeak_SetSynthCallback(self._callback)

    def _collect(self, wav, num_samples: int, _events) -> int:
        if num_samples > 0 and wav:
            self._buffer.extend(ctypes.string_at(wav, num_samples * 2))
        return 0

    def synthesize(self, text: str, voice: str) -> array:
        """Return S16LE mono samples at ``self.native_rate``."""
        if self._lib.espeak_SetVoiceByName(voice.encode()) != 0:
            raise RuntimeError(f"espeak voice not available: {voice!r}")
        self._buffer.clear()
        encoded = text.encode("utf-8")
        status = self._lib.espeak_Synth(
            encoded,
            len(encoded) + 1,
            0,
            self._POS_CHARACTER,
            0,
            self._CHARS_UTF8,
            None,
            None,
        )
        if status != 0:
            raise RuntimeError(f"espeak_Synth failed with status {status}")
        self._lib.espeak_Synchronize()
        if not self._buffer:
            raise RuntimeError(f"espeak produced no audio for {text!r}")
        return array("h", bytes(self._buffer))


def resample_linear(samples: array, src_rate: int, dst_rate: int) -> array:
    """Linear-interpolation resampler. Adequate for synthetic speech
    fixtures; not for production audio paths."""
    if src_rate == dst_rate:
        return samples
    n_out = int(len(samples) * dst_rate / src_rate)
    out = array("h", bytes(2 * n_out))
    ratio = src_rate / dst_rate
    last = len(samples) - 1
    for i in range(n_out):
        pos = i * ratio
        j = int(pos)
        if j >= last:
            out[i] = samples[last]
        else:
            frac = pos - j
            out[i] = int(samples[j] * (1.0 - frac) + samples[j + 1] * frac)
    return out


def mix_noise(samples: array, snr_db: float, seed: int) -> array:
    """Add seeded Gaussian noise at the given signal-to-noise ratio."""
    rms = math.sqrt(sum(s * s for s in samples) / len(samples))
    sigma = rms / (10.0 ** (snr_db / 20.0))
    rng = random.Random(seed)
    noisy = array("h", bytes(2 * len(samples)))
    for i, s in enumerate(samples):
        noisy[i] = max(-32768, min(32767, int(s + rng.gauss(0.0, sigma))))
    return noisy


def write_wav(path: Path, samples: array, rate: int) -> float:
    """Write S16LE mono WAV; returns duration in seconds."""
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(samples.tobytes())
    return len(samples) / rate


def generate(
    out_dir: Path,
    clip_specs: tuple[ClipSpec, ...] = CLIP_SPECS,
    noise_specs: tuple[NoiseSpec, ...] = NOISE_SPECS,
) -> Path:
    """Generate the corpus into ``out_dir``; returns the manifest path."""
    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    synth = EspeakSynthesizer()

    entries = []
    clean_samples: dict[str, tuple[ClipSpec, array]] = {}

    def add_entry(clip_id, spec: ClipSpec, samples: array, category: str) -> None:
        duration = write_wav(audio_dir / f"{clip_id}.wav", samples, TARGET_RATE)
        entries.append(
            {
                "id": clip_id,
                "path": f"audio/{clip_id}.wav",
                "text": spec.text,
                "language": spec.language,
                "category": category,
                "duration_seconds": round(duration, 3),
                "sample_rate_hz": TARGET_RATE,
                "channels": 1,
                "source": "synthetic:espeak-ng",
                "license": "CC0-1.0",
                "voice": spec.voice,
            }
        )
        print(f"  {clip_id:<24} {category:<12} {duration:6.2f}s")

    for spec in clip_specs:
        samples = resample_linear(
            synth.synthesize(spec.text, spec.voice), synth.native_rate, TARGET_RATE
        )
        clean_samples[spec.id] = (spec, samples)
        add_entry(spec.id, spec, samples, spec.category)

    for noise in noise_specs:
        spec, samples = clean_samples[noise.base_id]
        noisy = mix_noise(samples, noise.snr_db, NOISE_SEED)
        add_entry(noise.id, spec, noisy, "noise")

    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generator": "dev/generate_fixtures.py",
                "clips": entries,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    return manifest_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=REPO_ROOT / "server" / "fixtures",
        help="output corpus directory (default: <repo>/server/fixtures)",
    )
    args = parser.parse_args()
    manifest = generate(args.out)
    print(f"\nwrote {manifest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
