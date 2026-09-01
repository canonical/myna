#!/usr/bin/env python3
"""Encoder prefix-reuse divergence spike (perf T05).

    cd server && uv run python ../dev/spikes/parakeet_prefix_reuse.py \
        --json ../results/result.json

    # replay a saved sweep's tables without re-measuring anything:
    uv run python ../dev/spikes/parakeet_prefix_reuse.py --replay result.json

T05's question, asked directly of the model rather than of the streaming
loop: when the uncommitted window grows from N-k seconds to N seconds, can
the encoder output already computed for the first N-k seconds' worth of
frames be kept unchanged, so only the new tail needs encoding?

Two conditions are compared against a common target, per audio clip and per
trim amount ``k``:

  TARGET   -- the full N-second window's own encoder output, decoded using
              only its first T frames (T = the frame count a fresh N-k
              second decode would produce). This is "what the model will
              eventually say about this audio once the window has grown to
              N seconds" -- the thing a correct cache would have to predict
              in advance from less audio.
  CASE A   -- "naive": encode the N-k second prefix in complete isolation,
              exactly what ``_chunked_partial`` does on every tick today.
              Confounds two effects: the nemo128 preprocessor's
              utterance-global CMVN (mean/variance computed over whatever
              window is currently in scope, so features shift with window
              length -- see SPEC's "the obstacle, stated honestly" and
              dev/parakeet/fetch_parakeet_onnx.py) AND the encoder's own truncated
              receptive field.
  CASE B   -- "CMVN held fixed": slice the FULL window's own post-CMVN
              feature array down to the first T frames and run only the
              encoder on that slice. Because this is a literal slice of the
              same array used to produce TARGET's input, the per-frame input
              to the encoder is byte-identical between this and the eventual
              full decode for every frame position < T. Any remaining
              divergence from TARGET is caused by the encoder itself --
              self-attention and conv modules processing a shorter sequence
              -- with the CMVN confound removed by construction.

For each condition: per-frame cosine similarity against TARGET's encoder
output (bucketed by distance from the prefix's right edge, since a boundary-
local effect should decay with distance and a global effect should not), and
word-error-rate of the *decoded text* against TARGET's decoded text (the
tolerance that actually matters -- a frame can drift numerically without
ever flipping an argmax).

Clips: a clean single utterance, a clip already known to sit near the
encoder's documented collapse boundary (``stream-2277-01``, see
``dev/parakeet/fetch_parakeet_onnx.py``'s module docstring), and a 60-second window of
the pause-free longform clip (T03/T04's "no natural pauses, window grows
unbounded" case) -- the exact regime a working cache would need to pay off
in. Per this session's safety note, no clip is ever loaded past 60 seconds
of audio; this is plain forward inference through the public ``.run()`` API
(the same calls ``dev/parakeet/bench_parakeet.py`` and T04's sweep already make), not
calibration or activation dumping, so no extra memory guard is required
beyond that duration cap.

Before measuring, this calls ``bench_guard.check()`` (perf T02), same as the
other dev benchmarks, even though this spike does not claim a latency number
-- the run should still happen on a machine known not to be silently
throttled, since a throttled machine can also corrupt numerics indirectly
(swapping, thermal throttling changing which BLAS kernel path gets used is
not expected, but there is no reason to take the risk on a spike whose whole
point is to be trustworthy evidence).
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import wave
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives here, not in dev/spikes/

import bench_guard  # noqa: E402

from myna.testbed.metrics import word_error_rate  # noqa: E402
from myna.testbed.parakeet import (  # noqa: E402
    FRAME_S,
    PARAKEET_RATE,
    _default_model_dir,
    _detokenize,
    _encoder_threads,
    _ParakeetOnnx,
)

# Never load more than this much audio, regardless of what a clip contains.
# See the module docstring and this session's safety note: an earlier task
# (T06) crashed the host outright by driving a memory-hungry ONNX operation
# over an unexpectedly long clip with no cap. This spike does plain forward
# inference (no calibration, no activation dumping), but the cap costs
# nothing and removes the risk category entirely.
MAX_CLIP_SECONDS = 60.0

# Cosine similarity above which a frame's representation is considered
# "stabilized" -- close enough to identical that it would not be expected to
# perturb an argmax anywhere downstream. Chosen well above ordinary int8
# quantization noise (T09 found ORT's int8 path exactly reproducible run to
# run) so a value below this reflects a real representational difference,
# not measurement noise.
STABLE_COSINE = 0.999

# Frames within this distance of the prefix's right edge are "near-edge";
# beyond it, "far-interior". 20 frames = 1.6s, chosen to be generously past
# any plausible local attention/conv kernel width (FastConformer conv
# modules are kernel-31, i.e. +/-15 frames of local context) so a divergence
# that persists past this distance cannot be explained by ordinary local
# receptive field and must be attributed to something with longer range.
NEAR_EDGE_FRAMES = 20


def _load_wav(path: Path, max_seconds: float = MAX_CLIP_SECONDS) -> np.ndarray:
    with wave.open(str(path), "rb") as wav:
        if wav.getcomptype() != "NONE":
            raise ValueError(f"{path}: only uncompressed PCM WAV is supported")
        if wav.getnchannels() != 1 or wav.getframerate() != PARAKEET_RATE:
            raise ValueError(
                f"{path}: need {PARAKEET_RATE} Hz mono, got "
                f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
            )
        n = min(wav.getnframes(), int(max_seconds * wav.getframerate()))
        pcm = np.frombuffer(wav.readframes(n), dtype=np.int16)
    return pcm.astype(np.float32) / 32768.0


def _preprocess(model: _ParakeetOnnx, samples: np.ndarray) -> tuple[np.ndarray, int]:
    waveforms = samples.reshape(1, -1).astype(np.float32)
    waveforms_lens = np.array([samples.shape[0]], dtype=np.int64)
    features, features_lens = model._preprocessor.run(
        ["features", "features_lens"], {"waveforms": waveforms, "waveforms_lens": waveforms_lens}
    )
    return features, int(features_lens[0])


def _encode(model: _ParakeetOnnx, features: np.ndarray, flen: int) -> tuple[np.ndarray, int]:
    """Encode the first ``flen`` feature frames. Returns ``[1, T, 1024]``
    (already transposed, matching what ``_decode_sequence`` consumes) and the
    encoder's own reported output length."""
    fl = np.array([flen], dtype=np.int64)
    out, out_lens = model._encoder.run(
        ["outputs", "encoded_lengths"],
        {"audio_signal": np.ascontiguousarray(features[:, :, :flen]), "length": fl},
    )
    return np.ascontiguousarray(np.transpose(out, (0, 2, 1))), int(out_lens[0])


def _decode_text(model: _ParakeetOnnx, enc_out: np.ndarray, enclen: int) -> str:
    tokens, _ = model._decode_sequence(enc_out[0], enclen)
    return _detokenize(tokens)


def _cosine_per_frame(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """``a``, ``b``: ``[T, 1024]``. One cosine similarity per frame."""
    num = np.einsum("ij,ij->i", a, b)
    denom = np.linalg.norm(a, axis=1) * np.linalg.norm(b, axis=1) + 1e-9
    return num / denom


# Distance-from-right-edge bins (frames), for the "how many frames of right
# context are needed before output stabilises" question. Fine near the edge
# (FastConformer's depthwise conv is kernel-31, so +/-15 frames of local
# smearing is expected there), coarse further out where the question is
# instead "does it ever fully settle."
DISTANCE_BINS = [(0, 5), (5, 10), (10, 20), (20, 40), (40, 80), (80, 160), (160, 320), (320, 10**9)]


def _distance_profile(sims: np.ndarray) -> list[dict]:
    """``sims[i]`` is frame ``i``'s cosine similarity to TARGET; frame
    ``len(sims)-1`` is the prefix's right edge (distance 0)."""
    t = len(sims)
    distance = np.arange(t)[::-1]
    profile = []
    for lo, hi in DISTANCE_BINS:
        mask = (distance >= lo) & (distance < hi)
        if not mask.any():
            continue
        bucket = sims[mask]
        profile.append(
            {
                "distance_frames": [lo, hi if hi < 10**9 else None],
                "n": int(mask.sum()),
                "median": float(np.median(bucket)),
                "min": float(bucket.min()),
            }
        )
    return profile


@dataclass
class Cell:
    clip: str
    audio_seconds: float
    k_seconds: float
    prefix_frames: int
    prefix_seconds: float
    target_text: str
    case_a_text: str
    case_b_text: str
    case_a_wer_vs_target: float
    case_b_wer_vs_target: float
    case_a_sims_near_edge: list[float]
    case_a_sims_far: list[float]
    case_b_sims_near_edge: list[float]
    case_b_sims_far: list[float]
    case_b_distance_profile: list[dict]
    cmvn_mean_shift_l2: float
    cmvn_std_shift_l2: float

    def summary_row(self) -> dict:
        def stat(values: list[float]) -> dict | None:
            if not values:
                return None
            return {
                "median": statistics.median(values),
                "min": min(values),
                "frac_stable": sum(v >= STABLE_COSINE for v in values) / len(values),
            }

        return {
            "clip": self.clip,
            "audio_seconds": self.audio_seconds,
            "k_seconds": self.k_seconds,
            "prefix_seconds": self.prefix_seconds,
            "prefix_frames": self.prefix_frames,
            "case_a_wer_vs_target": self.case_a_wer_vs_target,
            "case_b_wer_vs_target": self.case_b_wer_vs_target,
            "case_a_near_edge": stat(self.case_a_sims_near_edge),
            "case_a_far_interior": stat(self.case_a_sims_far),
            "case_b_near_edge": stat(self.case_b_sims_near_edge),
            "case_b_far_interior": stat(self.case_b_sims_far),
            "case_b_distance_profile": self.case_b_distance_profile,
            "cmvn_mean_shift_l2": self.cmvn_mean_shift_l2,
            "cmvn_std_shift_l2": self.cmvn_std_shift_l2,
            "target_text": self.target_text,
            "case_a_text": self.case_a_text,
            "case_b_text": self.case_b_text,
        }


def run_cell(model: _ParakeetOnnx, clip: str, samples: np.ndarray, k: float) -> Cell | None:
    audio_seconds = len(samples) / PARAKEET_RATE
    if k >= audio_seconds - 1.0:
        return None  # prefix too short to be meaningful

    features_full, flen_full = _preprocess(model, samples)
    enc_full, enclen_full = _encode(model, features_full, flen_full)

    prefix_samples = samples[: int((audio_seconds - k) * PARAKEET_RATE)]
    features_naive, flen_naive = _preprocess(model, prefix_samples)
    enc_naive, enclen_naive = _encode(model, features_naive, flen_naive)
    enc_caseb, enclen_caseb = _encode(model, features_full, flen_naive)
    assert enclen_naive == enclen_caseb, (
        f"frame count mismatch: naive={enclen_naive} case_b={enclen_caseb} "
        "(same input length should always produce the same encoder output length)"
    )
    t = min(enclen_naive, enclen_full)
    if t < NEAR_EDGE_FRAMES + 5:
        return None  # too short to split into near-edge / far-interior meaningfully

    target_full = enc_full[0, :t]
    naive = enc_naive[0, :t]
    caseb = enc_caseb[0, :t]

    sims_a = _cosine_per_frame(naive, target_full)
    sims_b = _cosine_per_frame(caseb, target_full)
    near = slice(t - NEAR_EDGE_FRAMES, t)
    far = slice(0, t - NEAR_EDGE_FRAMES)

    target_text = _decode_text(model, enc_full[:, :t, :], t)
    case_a_text = _decode_text(model, enc_naive, t)
    case_b_text = _decode_text(model, enc_caseb, t)

    # CMVN shift: per-mel-channel mean/std computed by the preprocessor over
    # the naive prefix window versus over the full window, restricted to the
    # region both windows share. Quantifies the confound Case B was built to
    # remove, independent of anything encoder-related.
    feat_full_region = features_full[0, :, :flen_naive]
    feat_naive_region = features_naive[0, :, :flen_naive]
    # Reconstructing separate mean/std per window from already-normalized
    # features isn't possible without the pre-CMVN signal; instead compare
    # the normalized feature arrays directly over the shared region, which
    # is the CMVN shift as it actually reaches the encoder.
    mean_shift = float(
        np.linalg.norm(feat_full_region.mean(axis=1) - feat_naive_region.mean(axis=1))
    )
    std_shift = float(np.linalg.norm(feat_full_region.std(axis=1) - feat_naive_region.std(axis=1)))

    return Cell(
        clip=clip,
        audio_seconds=audio_seconds,
        k_seconds=k,
        prefix_frames=t,
        prefix_seconds=t * FRAME_S,
        target_text=target_text,
        case_a_text=case_a_text,
        case_b_text=case_b_text,
        case_a_wer_vs_target=word_error_rate(target_text, case_a_text).rate,
        case_b_wer_vs_target=word_error_rate(target_text, case_b_text).rate,
        case_a_sims_near_edge=sims_a[near].tolist(),
        case_a_sims_far=sims_a[far].tolist(),
        case_b_sims_near_edge=sims_b[near].tolist(),
        case_b_sims_far=sims_b[far].tolist(),
        case_b_distance_profile=_distance_profile(sims_b),
        cmvn_mean_shift_l2=mean_shift,
        cmvn_std_shift_l2=std_shift,
    )


def _print_row(row: dict) -> None:
    def fmt(stat: dict | None) -> str:
        if stat is None:
            return "n/a"
        return f"med={stat['median']:.4f} stable%={100 * stat['frac_stable']:.0f}"

    print(
        f"{row['clip']:<28} k={row['k_seconds']:<5} prefix={row['prefix_seconds']:.2f}s "
        f"({row['prefix_frames']} frames)"
    )
    print(
        f"  case A (naive)     far={fmt(row['case_a_far_interior'])}  "
        f"near_edge={fmt(row['case_a_near_edge'])}  wer_vs_target={row['case_a_wer_vs_target']:.3f}"
    )
    print(
        f"  case B (cmvn-fix)  far={fmt(row['case_b_far_interior'])}  "
        f"near_edge={fmt(row['case_b_near_edge'])}  wer_vs_target={row['case_b_wer_vs_target']:.3f}"
    )
    print(
        f"  cmvn shift (naive vs full, shared region): mean_l2={row['cmvn_mean_shift_l2']:.4f} "
        f"std_l2={row['cmvn_std_shift_l2']:.4f}"
    )
    prof = row.get("case_b_distance_profile") or []
    if prof:
        bins = "  ".join(
            f"[{b['distance_frames'][0]},"
            f"{b['distance_frames'][1] or 'inf'}):"
            f"med={b['median']:.3f}/min={b['min']:.3f}"
            for b in prof
        )
        print(f"  case B distance-from-edge profile: {bins}")


@dataclass
class ClipSpec:
    name: str
    path: Path
    ks: list[float] = field(default_factory=lambda: [0.5, 1.0, 2.0, 4.0, 8.0, 16.0])


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--model", type=str, default=None)
    ap.add_argument("--json", type=Path, default=None, help="write every cell as one JSON line")
    ap.add_argument("--force", action="store_true", help="measure even with guard violations")
    ap.add_argument("--replay", type=Path, default=None, help="print rows from a saved JSONL file")
    args = ap.parse_args()

    if args.replay is not None:
        for line in args.replay.read_text(encoding="utf-8").splitlines():
            if line.strip():
                _print_row(json.loads(line))
        return

    pre_violations = bench_guard.check()
    for v in pre_violations:
        print(v, file=sys.stderr)
    hard_pre = [v for v in pre_violations if v.severity == bench_guard.HARD]
    if hard_pre and not args.force:
        print(
            "refusing to measure on a contaminated machine (see violations above); "
            "fix the environment or pass --force",
            file=sys.stderr,
        )
        raise SystemExit(1)

    model_dir = args.model or _default_model_dir()
    model = _ParakeetOnnx(model_dir, encoder_threads=_encoder_threads())

    corpus = REPO_ROOT / "corpus" / "real" / "audio"
    clips = [
        ClipSpec("librispeech-422-clean", corpus / "librispeech-422-122949-0001.wav"),
        ClipSpec("stream-2277-01-collapse-adjacent", corpus / "streams" / "stream-2277-01.wav"),
        ClipSpec(
            "longform-60s-no-pauses",
            corpus / "librispeech-3081-166546-longform.wav",
            ks=[0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
        ),
    ]

    for spec in clips:
        samples = _load_wav(spec.path)
        for k in spec.ks:
            cell = run_cell(model, spec.name, samples, k)
            if cell is None:
                continue
            row = cell.summary_row()
            row["measured_at"] = datetime.now(UTC).isoformat()
            if hard_pre:
                row["environment"] = "dirty"
            _print_row(row)
            if args.json:
                if row.get("environment") == "dirty" and not args.force:
                    print("refusing to write a dirty record without --force", file=sys.stderr)
                    raise SystemExit(1)
                args.json.parent.mkdir(parents=True, exist_ok=True)
                with args.json.open("a", encoding="utf-8") as fh:
                    fh.write(json.dumps(row) + "\n")


if __name__ == "__main__":
    main()
