#!/usr/bin/env python3
"""Whisper encoder-context truncation: does a shorter mel pay? (whisper perf WP03)

    cd server && uv run --extra whisper python ../dev/spikes/whisper_audio_ctx.py
    cd server && uv run --extra whisper python ../dev/spikes/whisper_audio_ctx.py \
        --model base --frames 3000 2000 --clips 20

**The answer is no, and this exists so nobody has to find that out twice.**

Whisper's encoder input is padded to a fixed 3000 mel frames (30 s) by
``faster_whisper.audio.pad_or_trim``, so encoding 5 s costs what encoding 29 s
costs. CTranslate2 will happily encode a shorter mel - 20x faster at 600
frames - which makes truncation look like the obvious win for streaming, where
the same short window is re-encoded every tick.

End to end it is a large loss in *both* directions. Whisper's positional
embeddings are learned for 1500 encoder output positions, so a truncated
context is not a smaller version of the same input but a distribution the
decoder never saw. The decode degenerates, that trips faster-whisper's
compression-ratio and log-probability thresholds, and the temperature-fallback
ladder re-decodes the segment up to six times. Measured on tiny/int8 over
`corpus/real/manifest-balanced.json` (2026-09-02): 20 s of context costs
6.21% -> 21.44% WER and runs **2.3x slower**; 10 s costs 108% WER and 7.5x
slower, with 73% of segments taking the ladder against 13% at baseline.

Write-up: `docs/project-plan.md` T83.

Two things must move together or the experiment measures its own bug:
``pad_or_trim``'s length *and* ``feature_extractor.nb_max_frames``. Patch only
the first and the seek loop still advances 30 s per segment while the encoder
sees less, silently skipping the audio in between - which reads as a model
accuracy loss and is not one.
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
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives here

import bench_guard  # noqa: E402
from myna.testbed.metrics import normalize, word_error_rate  # noqa: E402
from myna.testbed.whisper import batch_decode_options  # noqa: E402

WHISPER_RATE = 16_000
DEFAULT_FRAMES = (3000, 2000, 1500, 1000)
SHIPPED_COMPUTE_TYPE = {"tiny": "int8"}  # everything else float32; see models/*/model.yaml


def _load(path: Path) -> np.ndarray:
    with wave.open(str(path)) as w:
        pcm = w.readframes(w.getnframes())
    return np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--model", default="tiny")
    ap.add_argument("--compute-type", default=None)
    ap.add_argument("--corpus", type=Path, default=REPO_ROOT / "corpus" / "real")
    ap.add_argument("--manifest", default="manifest-balanced.json")
    ap.add_argument("--clips", type=int, default=0, help="limit to the first N clips (0 = all)")
    ap.add_argument("--frames", type=int, nargs="+", default=list(DEFAULT_FRAMES))
    ap.add_argument("--force", action="store_true", help="measure despite guard violations")
    args = ap.parse_args()

    violations = bench_guard.check(bench_guard.PROFILES["whisper"])
    for v in violations:
        print(v, file=sys.stderr)
    if [v for v in violations if v.severity == bench_guard.HARD] and not args.force:
        raise SystemExit("refusing to measure on a contaminated machine; --force to override")

    import faster_whisper.transcribe as fwt
    from faster_whisper import WhisperModel
    from faster_whisper.transcribe import WhisperModel as _WM

    compute_type = args.compute_type or SHIPPED_COMPUTE_TYPE.get(args.model, "float32")
    model = WhisperModel(args.model, device="cpu", compute_type=compute_type)

    clips = json.loads((args.corpus / args.manifest).read_text(encoding="utf-8"))["clips"]
    if args.clips:
        clips = clips[: args.clips]
    audio = [(c, _load(args.corpus / c["path"])) for c in clips]
    total_audio = sum(len(a) for _, a in audio) / WHISPER_RATE

    state = {"frames": 3000, "encodes": 0, "fallbacks": 0}
    orig_pad, orig_encode, orig_fallback = fwt.pad_or_trim, _WM.encode, _WM.generate_with_fallback
    fwt.pad_or_trim = lambda a, length=3000, *, axis=-1: orig_pad(a, state["frames"], axis=axis)

    def counted_encode(self, features, *a, **kw):
        state["encodes"] += 1
        return orig_encode(self, features, *a, **kw)

    def counted_fallback(self, *a, **kw):
        out = orig_fallback(self, *a, **kw)
        if out[2]:  # winning temperature != 0: the ladder fired on this segment
            state["fallbacks"] += 1
        return out

    _WM.encode, _WM.generate_with_fallback = counted_encode, counted_fallback

    print(
        f"{args.model}/{compute_type}: {len(clips)} clips, {total_audio:.1f}s, "
        f"longest {max(len(a) for _, a in audio) / WHISPER_RATE:.1f}s"
    )
    header = ("frames", "ctx_s", "WER%", "wall_s", "RTF", "speedup", "encodes", "ladder")
    print("".join(f"{h:>9}" for h in header))

    baseline_wall = None
    try:
        for frames in args.frames:
            state.update(frames=frames, encodes=0, fallbacks=0)
            # Segmentation must agree with the encoder; see the module docstring.
            model.feature_extractor.nb_max_frames = frames
            model.feature_extractor.n_samples = frames * model.feature_extractor.hop_length

            errors = ref_words = 0
            t0 = time.perf_counter()
            for clip, samples in audio:
                segments, _info = model.transcribe(
                    samples, **batch_decode_options(clip["language"], None)
                )
                hypothesis = "".join(s.text for s in segments)
                # Micro-average (edits summed over reference words), matching
                # dev/aggregate.py. Concatenating every clip into one string
                # instead lets the aligner match across clip boundaries.
                rate = word_error_rate(clip["text"], hypothesis)
                errors += rate.substitutions + rate.deletions + rate.insertions
                ref_words += len(normalize(clip["text"]).split())
            wall = time.perf_counter() - t0
            baseline_wall = baseline_wall or wall
            row = (
                f"{frames:>9}",
                f"{frames / 100:>9.0f}",
                f"{100 * errors / ref_words:>9.2f}",
                f"{wall:>9.2f}",
                f"{wall / total_audio:>9.4f}",
                f"{baseline_wall / wall:>8.2f}x",
                f"{state['encodes']:>9}",
                f"{state['fallbacks']:>9}",
            )
            print("".join(row))
    finally:
        fwt.pad_or_trim = orig_pad
        _WM.encode, _WM.generate_with_fallback = orig_encode, orig_fallback


if __name__ == "__main__":
    main()
