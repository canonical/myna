#!/usr/bin/env python3
"""Sliding-window encoder collapse probe (perf T06).

    cd server && uv run python ../dev/parakeet/collapse_probe.py \
        --model-dir ~/.cache/myna/models/parakeet-tdt-0.6b-v3-int8 \
        --json ../results/collapse_before.json

    uv run python ../dev/parakeet/collapse_probe.py \
        --model-dir /tmp/requant-model-dir \
        --json ../results/collapse_after.json

Replicates the methodology behind the 2026-08-28 baseline's
"11.5% of 486 sliding windows (3-16s)" figure, cited in
``server/src/myna/testbed/parakeet.py``'s module docstring: real speech,
concatenated into one long stream so window boundaries land mid-utterance
(collapse is a window-boundary artifact, not a per-clip one), sampled at many
start offsets per window length.

A window "collapses" by the exact definition production uses to trigger its
own retry (``_transcribe_guarded`` in parakeet.py): raw token count under
``_COLLAPSE_WORDS_PER_SECOND`` (0.5) tokens per second of window. This calls
``_ParakeetOnnx.transcribe`` directly (not ``transcribe_text``/
``_transcribe_guarded``) so the retry mitigation cannot mask a collapse.

Stream: the 12 real corpus/english/audio/librispeech-2277-149896-* segments
(same speaker and passage, ~51.7 s) concatenated in filename order, followed
by the first 65 s of the corpus's one long-form clip
(librispeech-3081-166546-longform.wav) -- both are genuine continuous
narration, not synthetic tone/noise, and concatenating gives window
boundaries that do not all coincide with real utterance starts.

Primary sweep: 3-16 s in 1 s steps, 3 s offset stride -- same range as the
cited baseline figure, for direct before/after comparability. Secondary
sweep: 20-60 s in 5 s steps, 15 s offset stride -- coarser, but the acceptance
bar is the *full* window-length sweep, and production windows run up to
SC_FORCE_CUT_S (60 s), not just 16 s.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import wave
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

PARAKEET_RATE = 16_000
_COLLAPSE_WORDS_PER_SECOND = 0.5  # must match myna.testbed.parakeet's constant

STREAM_A_CLIPS = [
    "librispeech-2277-149896-0005",
    "librispeech-2277-149896-0006",
    "librispeech-2277-149896-0007",
    "librispeech-2277-149896-0012",
    "librispeech-2277-149896-0015",
    "librispeech-2277-149896-0018",
    "librispeech-2277-149896-0021",
    "librispeech-2277-149896-0026",
    "librispeech-2277-149896-0027",
    "librispeech-2277-149896-0030",
    "librispeech-2277-149896-0033",
    "librispeech-2277-149896-0034",
]
STREAM_B_CLIP = "librispeech-3081-166546-longform"
STREAM_B_SECONDS = 65.0

PRIMARY_LENGTHS = list(range(3, 17))  # 3..16 inclusive
PRIMARY_OFFSET_STRIDE = 3.0
SECONDARY_LENGTHS = list(range(20, 61, 5))  # 20..60 inclusive
SECONDARY_OFFSET_STRIDE = 15.0


def _load_wav(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wav:
        pcm = np.frombuffer(wav.readframes(wav.getnframes()), dtype=np.int16)
    return pcm.astype(np.float32) / 32768.0


def _build_stream(corpus_dir: Path) -> np.ndarray:
    parts = [_load_wav(corpus_dir / f"{name}.wav") for name in STREAM_A_CLIPS]
    b = _load_wav(corpus_dir / f"{STREAM_B_CLIP}.wav")
    parts.append(b[: int(STREAM_B_SECONDS * PARAKEET_RATE)])
    return np.concatenate(parts)


def _windows(stream_len_s: float, lengths: list[int], stride: float) -> list[tuple[float, int]]:
    out = []
    for length in lengths:
        start = 0.0
        while start + length <= stream_len_s:
            out.append((start, length))
            start += stride
    return out


def run_probe(model_dir: str, corpus_dir: Path, threads: int) -> dict:
    from myna.testbed.parakeet import _ParakeetOnnx

    stream = _build_stream(corpus_dir)
    stream_len_s = len(stream) / PARAKEET_RATE
    plan = _windows(stream_len_s, PRIMARY_LENGTHS, PRIMARY_OFFSET_STRIDE) + _windows(
        stream_len_s, SECONDARY_LENGTHS, SECONDARY_OFFSET_STRIDE
    )

    model = _ParakeetOnnx(model_dir, encoder_threads=threads)
    # Warm up: first call pays one-time session/arena setup cost, not
    # representative of steady-state collapse behaviour.
    model.transcribe(stream[: 3 * PARAKEET_RATE])

    results = []
    collapsed = 0
    t0 = time.perf_counter()
    for start_s, length in plan:
        i0 = int(start_s * PARAKEET_RATE)
        i1 = i0 + int(length * PARAKEET_RATE)
        window = stream[i0:i1]
        tokens, _ = model.transcribe(window)
        is_collapsed = len(tokens) < _COLLAPSE_WORDS_PER_SECOND * length
        if is_collapsed:
            collapsed += 1
        results.append(
            {
                "start_s": round(start_s, 2),
                "length_s": length,
                "tokens": len(tokens),
                "collapsed": is_collapsed,
            }
        )
    elapsed = time.perf_counter() - t0

    return {
        "model_dir": str(model_dir),
        "stream_seconds": stream_len_s,
        "n_windows": len(plan),
        "n_collapsed": collapsed,
        "collapse_rate": collapsed / len(plan) if plan else 0.0,
        "probe_wall_seconds": elapsed,
        "windows": results,
    }


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--model-dir", required=True)
    ap.add_argument(
        "--corpus-dir",
        type=Path,
        default=REPO_ROOT / "corpus" / "real" / "audio",
    )
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--json", type=Path, required=True)
    args = ap.parse_args()

    summary = run_probe(args.model_dir, args.corpus_dir, args.threads)
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(summary, indent=2))

    print(f"model_dir: {summary['model_dir']}")
    print(f"stream: {summary['stream_seconds']:.1f}s, {summary['n_windows']} windows probed")
    print(
        f"collapsed: {summary['n_collapsed']}/{summary['n_windows']} "
        f"({100 * summary['collapse_rate']:.1f}%)"
    )
    print(f"probe wall time: {summary['probe_wall_seconds']:.1f}s")


if __name__ == "__main__":
    main()
