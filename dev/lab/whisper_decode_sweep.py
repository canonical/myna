#!/usr/bin/env python3
"""Sweep faster-whisper compute types and decode parameters over a corpus.

Two questions this exists to answer, both raised by outside work and both
cheap to settle with numbers instead of argument:

1. **Is an INT8 Whisper worth shipping as its own component?**
   RedHatAI publishes `whisper-tiny-quantized.w8a8` (GPTQ/SmoothQuant INT8
   weights *and* activations, compressed-tensors format, vLLM runtime). We
   already quantize at load time: CTranslate2 takes `compute_type=int8` on the
   same float weights. `--compute-types` measures what that costs us on the
   real corpus, which is the number the w8a8 card's "recovery %" is claiming.

2. **Do OpenWhispr's anti-hallucination decoder thresholds help us?**
   They report the whisper.cpp pair `entropy_thold 2.8 / logprob_thold -1.25`
   (from 2.4 / -1.0) cutting hallucinated tails from 2.25% to 0.06% over 4,814
   real dictations. faster-whisper's analogues are
   `compression_ratio_threshold` and `log_prob_threshold`, plus
   `condition_on_previous_text`, the usual repetition-loop source.
   `--decode-configs` sweeps those.

The adapter is bypassed on purpose: this measures the *model and decoder*, not
the session contract, so it is a lab tool and not a bench target. Snap-level
numbers come from dev/matrix.py.

    uv run --extra whisper python dev/lab/whisper_decode_sweep.py \
        --manifest corpus/real/manifest-balanced.json --model tiny \
        --compute-types float32 int8 int8_float32
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import wave
from dataclasses import asdict, dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
# The import below needs this path first, which is what E402 is waived for.
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

from myna.testbed.metrics import character_error_rate, word_error_rate  # noqa: E402
from myna.testbed.whisper import batch_decode_options  # noqa: E402

# Named decode configurations. "baseline" is exactly what the shipped adapter
# passes today; the rest are the hypotheses under test.
#
# **The polarity trap.** OpenWhispr's constants cannot be ported across by
# value, because faster-whisper's thresholds do not mean what whisper.cpp's
# identically-named ones mean. faster-whisper decides to drop a segment as
# silence like this (transcribe.py, `no voice activity check`):
#
#     should_skip = no_speech_prob > no_speech_threshold
#     if avg_logprob > log_prob_threshold:
#         should_skip = False        # "high enough logprob, keep it anyway"
#
# so *lowering* `log_prob_threshold` un-skips more segments and produces
# **more** spurious output, while whisper.cpp's `logprob_thold` is a
# decode-failed trigger where lowering is also more permissive but feeds a
# temperature-fallback ladder instead of a silence gate. Likewise
# whisper.cpp's `entropy_thold` rejects when entropy is *below* it, so raising
# it is stricter - the opposite direction to faster-whisper's
# `compression_ratio_threshold`, which rejects when the ratio is *above* it.
#
# `openwhispr-1458-literal` transcribes their numbers unchanged so the trap is
# in the results table rather than in a footnote; the `silence-*` entries are
# their *intent* expressed in this library's polarity.
# ``shipped`` comes from the adapter itself rather than being retyped, so it
# cannot drift. Note it is NOT the same as ``baseline``: baseline is
# faster-whisper's own defaults, which is what this script measured before T71
# put ``log_prob_threshold=-0.5`` in the adapter. Keep both - baseline is what
# the recorded T70/T71 rows compare against, shipped is what users run.
_SHIPPED = {k: v for k, v in batch_decode_options("en", None).items() if k != "language"}

DECODE_CONFIGS: dict[str, dict] = {
    "baseline": {},
    "shipped": dict(_SHIPPED),
    # --- whisper perf WP06: what the shipped batch path never sets ---
    # beam_size defaults to 5. The adapter's own comment claims "5 ~= batch
    # quality, 1 ~= 5x cheaper" about a path that passes neither.
    "beam1": {**_SHIPPED, "beam_size": 1},
    # The six-step temperature-fallback ladder re-decodes a rejected segment up
    # to six times. A tail-latency mechanism, so judge it on p95, not the mean.
    "no-ladder": {**_SHIPPED, "temperature": [0.0]},
    "ladder-2": {**_SHIPPED, "temperature": [0.0, 0.2]},
    "beam1-no-ladder": {**_SHIPPED, "beam_size": 1, "temperature": [0.0]},
    # condition_on_previous_text=True is the usual repetition-loop source and
    # also serialises segments against each other.
    "shipped-no-condition": {**_SHIPPED, "condition_on_previous_text": False},
    "beam1-no-ladder-no-condition": {
        **_SHIPPED,
        "beam_size": 1,
        "temperature": [0.0],
        "condition_on_previous_text": False,
    },
    # Their constants, taken at face value. Expected to be worse, not better.
    "openwhispr-1458-literal": {
        "compression_ratio_threshold": 2.8,
        "log_prob_threshold": -1.25,
    },
    # The repetition-loop source. whisper.cpp's server exposes no equivalent.
    "no-condition": {"condition_on_previous_text": False},
    # Their intent, correctly signed: make the silence skip stick instead of
    # letting a confidently-decoded "You" un-skip it.
    "silence-strict": {"log_prob_threshold": -0.5},
    "silence-strict-nospeech": {"log_prob_threshold": -0.5, "no_speech_threshold": 0.4},
    # Stricter repetition rejection - the true analogue of entropy_thold 2.8.
    "repetition-strict": {"compression_ratio_threshold": 2.0},
    # faster-whisper's built-in Silero VAD. The most direct silence fix and the
    # one OpenWhispr deliberately switch **off** for dictation (their #1454:
    # VAD on pause-heavy speech strips the speech and leaves Whisper decoding
    # near-silence seeded with the prompt). Measured here on both axes.
    "vad": {"vad_filter": True},
    "vad+silence-strict": {"vad_filter": True, "log_prob_threshold": -0.5},
}


@dataclass
class Row:
    model: str
    compute_type: str
    decode_config: str
    excluded_categories: str
    clips: int
    wer: float
    cer: float
    audio_seconds: float
    decode_seconds: float
    rtf: float
    load_seconds: float
    # Per-clip decode latency. The temperature ladder is a tail mechanism -
    # it fires on a minority of segments and multiplies their cost - so a mean
    # hides exactly the thing under test (whisper perf WP06).
    p50_ms: float
    p95_ms: float
    max_ms: float
    # Segments where the ladder actually fired (winning temperature != 0).
    ladder_segments: int
    # A transcript far longer than its reference is the hallucinated-tail
    # signature; count clips where the hypothesis runs away.
    runaway_clips: int
    empty_clips: int


def read_wav(path: Path):
    import numpy as np

    with wave.open(str(path), "rb") as w:
        if w.getnchannels() != 1 or w.getsampwidth() != 2 or w.getframerate() != 16_000:
            raise ValueError(f"{path}: need 16 kHz mono S16LE")
        pcm = w.readframes(w.getnframes())
    return np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0


def load_clips(manifest: Path, limit: int | None, exclude_categories: set[str] = frozenset()):
    """Load clips, optionally dropping whole categories.

    Excluding ``long-form`` makes a run directly comparable with
    dev/lab/w8a8_probe.py, which must exclude it (transformers' plain
    ``generate`` decodes one 30 s window). Do not exclude it by default here:
    the shipped CTranslate2 path handles long form, and the chapter clip is
    where repetition and drift actually show up."""
    data = json.loads(manifest.read_text())
    clips = [c for c in data["clips"] if c.get("category") not in exclude_categories]
    if limit:
        clips = clips[:limit]
    root = manifest.parent
    return [(c, read_wav(root / c["path"])) for c in clips]


def sweep(model_size, compute_type, decode_name, clips, download_root, threads, excluded=""):
    from faster_whisper import WhisperModel

    t0 = time.perf_counter()
    model = WhisperModel(
        model_size,
        device="cpu",
        compute_type=compute_type,
        download_root=download_root,
        cpu_threads=threads or 0,
    )
    load_seconds = time.perf_counter() - t0

    overrides = DECODE_CONFIGS[decode_name]
    wer_edits = wer_ref = cer_edits = cer_ref = 0
    audio_seconds = decode_seconds = 0.0
    runaway = empty = 0
    per_clip_ms: list[float] = []

    # Count segments that took the temperature ladder. Patched on the class
    # for the duration of the sweep, the same way dev/whisper/bench_whisper.py
    # instruments the stage timeline, so the count comes from the real decode.
    from faster_whisper.transcribe import WhisperModel as _WM

    ladder = {"n": 0}
    original_fallback = _WM.generate_with_fallback

    def counted_fallback(self, *a, **kw):
        out = original_fallback(self, *a, **kw)
        if out[2]:  # winning temperature != 0
            ladder["n"] += 1
        return out

    _WM.generate_with_fallback = counted_fallback

    for clip, samples in clips:
        t0 = time.perf_counter()
        segments, _info = model.transcribe(samples, language="en", **overrides)
        text = "".join(s.text for s in segments).strip()
        elapsed = time.perf_counter() - t0
        per_clip_ms.append(elapsed * 1000)
        decode_seconds += elapsed
        audio_seconds += clip["duration_seconds"]

        w = word_error_rate(clip["text"], text)
        c = character_error_rate(clip["text"], text)
        wer_edits += w.substitutions + w.deletions + w.insertions
        wer_ref += w.reference_length
        cer_edits += c.substitutions + c.deletions + c.insertions
        cer_ref += c.reference_length
        if not text:
            empty += 1
        elif w.insertions > max(5, w.reference_length):
            runaway += 1

    _WM.generate_with_fallback = original_fallback
    ordered = sorted(per_clip_ms)
    label = Path(model_size).name if "/" in model_size else model_size
    return Row(
        model=label,
        compute_type=compute_type,
        decode_config=decode_name,
        excluded_categories=excluded,
        clips=len(clips),
        wer=round(wer_edits / wer_ref, 5) if wer_ref else 0.0,
        cer=round(cer_edits / cer_ref, 5) if cer_ref else 0.0,
        audio_seconds=round(audio_seconds, 2),
        decode_seconds=round(decode_seconds, 2),
        rtf=round(decode_seconds / audio_seconds, 4) if audio_seconds else 0.0,
        load_seconds=round(load_seconds, 2),
        p50_ms=round(ordered[len(ordered) // 2], 1) if ordered else 0.0,
        p95_ms=round(ordered[min(int(0.95 * len(ordered)), len(ordered) - 1)], 1)
        if ordered
        else 0.0,
        max_ms=round(ordered[-1], 1) if ordered else 0.0,
        ladder_segments=ladder["n"],
        runaway_clips=runaway,
        empty_clips=empty,
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--manifest",
        type=Path,
        default=REPO_ROOT / "corpus/real/manifest-balanced.json",
    )
    p.add_argument("--models", nargs="+", default=["tiny"])
    p.add_argument("--compute-types", nargs="+", default=["float32", "int8"])
    p.add_argument("--decode-configs", nargs="+", default=["baseline"])
    p.add_argument("--limit", type=int, default=None, help="first N clips only (smoke runs)")
    p.add_argument(
        "--exclude-categories",
        nargs="*",
        default=[],
        help="corpus categories to skip (see load_clips)",
    )
    p.add_argument("--threads", type=int, default=0, help="cpu_threads (0 = CTranslate2 default)")
    p.add_argument("--download-root", default=None)
    p.add_argument("--out", type=Path, default=REPO_ROOT / "results/whisper-decode-sweep.jsonl")
    args = p.parse_args()

    for name in args.decode_configs:
        if name not in DECODE_CONFIGS:
            p.error(f"unknown decode config {name!r}; have {sorted(DECODE_CONFIGS)}")

    clips = load_clips(args.manifest, args.limit, set(args.exclude_categories))
    print(
        f"{len(clips)} clips from {args.manifest} "
        f"(excluding {args.exclude_categories or 'nothing'})",
        file=sys.stderr,
    )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    rows: list[Row] = []
    with args.out.open("a") as fh:
        for model in args.models:
            for compute_type in args.compute_types:
                for decode_name in args.decode_configs:
                    tag = f"{model}/{compute_type}/{decode_name}"
                    print(f"--- {tag}", file=sys.stderr, flush=True)
                    row = sweep(
                        model,
                        compute_type,
                        decode_name,
                        clips,
                        args.download_root,
                        args.threads,
                        ",".join(sorted(args.exclude_categories)),
                    )
                    rows.append(row)
                    fh.write(json.dumps(asdict(row)) + "\n")
                    fh.flush()
                    print(
                        f"    WER {row.wer:.4f}  CER {row.cer:.4f}  RTF {row.rtf:.3f}"
                        f"  load {row.load_seconds:.1f}s  runaway {row.runaway_clips}",
                        file=sys.stderr,
                        flush=True,
                    )

    hdr = (
        f"{'model':<14} {'compute':<9} {'decode':<30} {'WER':>8} {'CER':>8} "
        f"{'RTF':>7} {'p50ms':>8} {'p95ms':>8} {'maxms':>8} {'ladder':>7} {'runaway':>8}"
    )
    print("\n" + hdr)
    print("-" * len(hdr))
    for r in rows:
        print(
            f"{r.model:<14} {r.compute_type:<9} {r.decode_config:<30} "
            f"{r.wer:>8.4f} {r.cer:>8.4f} {r.rtf:>7.3f} "
            f"{r.p50_ms:>8.0f} {r.p95_ms:>8.0f} {r.max_ms:>8.0f} "
            f"{r.ladder_segments:>7} {r.runaway_clips:>8}"
        )
    print(f"\nappended to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
