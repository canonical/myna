#!/usr/bin/env python3
"""Re-quantize the encoder's fp32 FFN down-projections to int8 (perf T06).

    cd server && uv run --extra parakeet python ../dev/parakeet/requantize_encoder.py \
        --model-dir ~/.cache/myna/models/parakeet-tdt-0.6b-v3-int8 \
        --out ../results/encoder-model.int8.requant.onnx \
        --calib-glob '../corpus/english/audio/*.wav' --calib-n 16

The 2026-08-28 baseline found `feed_forward*/linear2` (the
4096 -> 1024 FFN down-projection) running in fp32 in the shipped murmure
encoder while every neighbouring GEMM (including its own `linear1` sibling)
is int8. This inspects the graph to find exactly which nodes those are, and
re-quantizes only those nodes with the same scheme already used everywhere
else in the file (verified by inspecting an already-quantized sibling node,
`/layers.0/feed_forward1/linear2/MatMul_quant`, before writing this):

  - QOperator format (QLinearMatMul), not QDQ -- matches every other GEMM.
  - Weight: int8, per-channel (one scale per output channel), symmetric
    (zero_point is 0 for every channel of the sibling weight inspected).
  - Activation: uint8, per-tensor, asymmetric (non-zero zero_point observed).
  - MinMax calibration (ORT's default; the sibling's scale values show no
    sign of a more exotic calibration method).

Why only these nodes and not a blanket `quantize_static` over the whole
graph: `nodes_to_quantize` restricts quantization to exactly the fp32 nodes
found, so everything already-quantized, every LayerNorm, and every other
fp32 elementwise op is untouched byte-for-byte. Confirmed by the node-count
diff this script prints after quantizing.

**Why these nodes are fp32 in the first place -- read before trusting a
"just quantize it" instinct.** They are not scattered at random: of the 48
`feed_forward{1,2}/linear2` MatMuls (24 conformer layers x 2 macaron FFN
blocks), exactly 11 are fp32, and they cluster in the back half of the
network (layers 12, 13, 15, 16, 21, 22, 23 -- zero skips in layers 0-11, 14,
17-20). Every one of those 11, quantized or not, already has a SmoothQuant
rescale (`..._smooth_output`) applied ahead of it, meaning murmure's export
pipeline smoothed activation outliers into the weight for all 48 and then
still chose not to quantize 11 of them. That pattern -- concentrated in
deeper layers, smoothed but still left fp32 -- is the signature of a
per-layer accuracy check that rejected these specific layers, not of an
op-type allowlist or a shape restriction (linear1 and linear2 share no shape
distinction that would explain it: both are plain 2-D MatMuls, and linear2
on other layers with the identical [1,T,4096] x [4096,1024] shape ships
quantized). This does not by itself prove requantizing is unsafe -- the
collapse probe is what decides that -- but it is the reason this task
budgets for an honest "abandon" outcome rather than treating the fp32 nodes
as a simple oversight.

"""

from __future__ import annotations

import argparse
import glob
import sys
import wave
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

PARAKEET_RATE = 16_000


def _load_wav(path: str) -> np.ndarray:
    with wave.open(path, "rb") as wav:
        pcm = np.frombuffer(wav.readframes(wav.getnframes()), dtype=np.int16)
    return pcm.astype(np.float32) / 32768.0


# The encoder is never run on more than this in production: chunked commit
# hard-cuts at SC_FORCE_CUT_S (streaming/strategies.py). Calibrating past it
# is not just unrepresentative, it is dangerous: the augmented graph that
# onnxruntime's calibrator builds pins many more intermediate buffers alive
# for the whole forward pass than a plain run does (each instrumented tensor
# gains a second consumer, the inserted Reduce node, which can push its last
# use later in the schedule and block the arena from reusing it). At the
# corpus's one 302 s outlier -- 5x this ceiling, quadratic self-attention over
# ~3774 frames -- that measured 10+ GB peak RSS under calibration alone,
# enough to crash a 30 GB machine when run without a cap. A plain forward
# pass on the same clip only reached 3.7 GB, so the augmentation, not the
# model, is what turns sequence length into a memory cliff.
_MAX_CALIB_CLIP_SECONDS = 60.0


def _pick_calibration_clips(pattern: str, n: int) -> list[str]:
    """Evenly spaced across the duration distribution, not just the first N
    alphabetically -- collapse is window-length dependent, so calibration
    should see short and long windows alike. Capped at
    ``_MAX_CALIB_CLIP_SECONDS``: production audio never reaches the encoder
    longer than that, and calibrating past it risks the memory cliff above."""
    all_paths = sorted(glob.glob(pattern))
    if not all_paths:
        raise SystemExit(f"no calibration clips matched {pattern!r}")
    all_durations = {}
    for p in all_paths:
        with wave.open(p, "rb") as wav:
            all_durations[p] = wav.getnframes() / wav.getframerate()
    paths = [p for p in all_paths if all_durations[p] <= _MAX_CALIB_CLIP_SECONDS]
    skipped = [p for p in all_paths if p not in paths]
    if skipped:
        print(
            f"skipping {len(skipped)} calibration clip(s) over "
            f"{_MAX_CALIB_CLIP_SECONDS:.0f}s (production ceiling, see module docstring): "
            + ", ".join(f"{p} ({all_durations[p]:.0f}s)" for p in skipped)
        )
    if not paths:
        raise SystemExit(
            f"no calibration clips under {_MAX_CALIB_CLIP_SECONDS:.0f}s matched {pattern!r}"
        )
    durations = [all_durations[p] for p in paths]
    order = sorted(range(len(paths)), key=lambda i: durations[i])
    if n >= len(order):
        return paths
    idxs = [order[int(i * (len(order) - 1) / (n - 1))] for i in range(n)]
    return [paths[i] for i in sorted(set(idxs))]


def _find_fp32_linear2_nodes(model) -> list[str]:
    """The fp32 `feed_forward*/linear2` MatMul nodes -- the ones the baseline
    profiled at 2295 us/call against 379 us for their quantized neighbours.
    Matched by op_type + name pattern rather than hardcoded, so this stays
    correct if a future re-export changes which layers are skipped."""
    return [
        n.name
        for n in model.graph.node
        if n.op_type == "MatMul" and "feed_forward" in n.name and "linear2" in n.name
    ]


class _EncoderCalibrationReader:
    """Feeds the encoder's own preprocessor output back into it, over a
    handful of real clips spanning the duration range collapse is sensitive
    to -- the nemo128 preprocessor does utterance-global CMVN, so activation
    ranges genuinely shift with window length and calibration should see
    that shift, not just one clip length."""

    def __init__(self, preprocessor, clip_paths: list[str]) -> None:
        self._preprocessor = preprocessor
        self._clip_paths = clip_paths
        self._i = 0

    def get_next(self) -> dict | None:
        if self._i >= len(self._clip_paths):
            return None
        samples = _load_wav(self._clip_paths[self._i])
        self._i += 1
        waveforms = samples.reshape(1, -1).astype(np.float32)
        waveforms_lens = np.array([samples.shape[0]], dtype=np.int64)
        features, features_lens = self._preprocessor.run(
            ["features", "features_lens"],
            {"waveforms": waveforms, "waveforms_lens": waveforms_lens},
        )
        return {"audio_signal": features, "length": features_lens}

    def __iter__(self):
        return self

    def __next__(self):
        result = self.get_next()
        if result is None:
            raise StopIteration
        return result

    def rewind(self) -> None:
        self._i = 0


def requantize(
    model_dir: str,
    out_path: Path,
    calib_glob: str,
    calib_n: int,
    calibrate_method: str = "minmax",
    nodes: list[str] | None = None,
) -> dict:
    import onnx
    import onnxruntime as ort
    from onnxruntime.quantization import CalibrationMethod, QuantFormat, QuantType, quantize_static

    encoder_path = str(Path(model_dir) / "encoder-model.int8.onnx")
    model = onnx.load(encoder_path, load_external_data=True)
    targets = _find_fp32_linear2_nodes(model)
    if nodes is not None:
        unknown = sorted(set(nodes) - set(targets))
        if unknown:
            raise SystemExit(f"--nodes not in the fp32 linear2 set: {unknown}")
        targets = [t for t in targets if t in set(nodes)]
    if not targets:
        raise SystemExit(f"no fp32 feed_forward*/linear2 MatMul nodes found in {encoder_path}")
    print(f"found {len(targets)} fp32 feed_forward*/linear2 MatMul nodes:")
    for t in targets:
        print(f"  {t}")

    clip_paths = _pick_calibration_clips(calib_glob, calib_n)
    print(f"\ncalibrating on {len(clip_paths)} clips:")
    for p in clip_paths:
        print(f"  {p}")

    opts = ort.SessionOptions()
    opts.log_severity_level = 3
    opts.intra_op_num_threads = 1
    preprocessor = ort.InferenceSession(
        str(Path(model_dir) / "nemo128.onnx"), opts, providers=["CPUExecutionProvider"]
    )
    reader = _EncoderCalibrationReader(preprocessor, clip_paths)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    quantize_static(
        model_input=encoder_path,
        model_output=str(out_path),
        calibration_data_reader=reader,
        quant_format=QuantFormat.QOperator,
        op_types_to_quantize=["MatMul"],
        nodes_to_quantize=targets,
        per_channel=True,
        activation_type=QuantType.QUInt8,
        weight_type=QuantType.QInt8,
        calibrate_method=(
            CalibrationMethod.Percentile
            if calibrate_method == "percentile"
            else CalibrationMethod.MinMax
        ),
        # op_types_to_quantize also selects which nodes the MinMax calibrator
        # instruments (onnxruntime.quantization.quantize.quantize_static passes
        # it straight through as op_types_to_calibrate), which is every plain
        # "MatMul" node -- 83 of them here, 72 of them unrelated self_attn
        # matmuls -- not just the 11 nodes_to_quantize targets. The default
        # (non-moving-average) MinMax calibrator accumulates one reduced
        # min/max array per calibration run for every instrumented tensor
        # before reducing at the end; moving_average folds each run in
        # immediately instead, bounding memory to O(instrumented tensors)
        # regardless of calib_n or how many extra nodes op_types_to_quantize
        # happens to pull in.
        extra_options=(
            {"CalibMovingAverage": True}
            if calibrate_method == "minmax"
            # T13: clip the top/bottom 0.005% of the activation distribution
            # instead of taking the absolute min/max -- these 11 layers were
            # rejected by murmure's export *despite* SmoothQuant, i.e. their
            # activation outliers survive smoothing; percentile calibration
            # spends the 8-bit range on the mass instead of the outliers.
            else {"CalibPercentile": 99.995}
        ),
    )

    before = onnx.load(encoder_path, load_external_data=True)
    after = onnx.load(str(out_path), load_external_data=True)
    from collections import Counter

    before_ops = Counter(n.op_type for n in before.graph.node)
    after_ops = Counter(n.op_type for n in after.graph.node)
    remaining_fp32 = _find_fp32_linear2_nodes(after)
    summary = {
        "targets": targets,
        "calibration_clips": clip_paths,
        "before_ops": dict(before_ops),
        "after_ops": dict(after_ops),
        "remaining_fp32_linear2": remaining_fp32,
        "out_path": str(out_path),
        "out_size_bytes": out_path.stat().st_size,
        "in_size_bytes": Path(encoder_path).stat().st_size,
    }
    return summary


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--model-dir", required=True, help="staged parakeet model directory")
    ap.add_argument("--out", type=Path, required=True, help="output encoder .onnx path")
    ap.add_argument("--calib-glob", default=str(REPO_ROOT / "corpus" / "real" / "audio" / "*.wav"))
    ap.add_argument("--calib-n", type=int, default=16)
    ap.add_argument(
        "--calibrate-method",
        choices=["minmax", "percentile"],
        default="minmax",
        help="minmax reproduces T06; percentile is T13's outlier-clipping retry",
    )
    ap.add_argument(
        "--nodes",
        default=None,
        help="comma-separated subset of the fp32 linear2 node names (T13 per-node selectivity)",
    )
    args = ap.parse_args()

    summary = requantize(
        args.model_dir,
        args.out,
        args.calib_glob,
        args.calib_n,
        calibrate_method=args.calibrate_method,
        nodes=args.nodes.split(",") if args.nodes else None,
    )
    print("\nop_type counts:")
    ops = sorted(set(summary["before_ops"]) | set(summary["after_ops"]))
    for op in ops:
        b = summary["before_ops"].get(op, 0)
        a = summary["after_ops"].get(op, 0)
        marker = "  <-- changed" if a != b else ""
        print(f"  {op:<22}{b:>6} -> {a:<6}{marker}")
    print(f"\nremaining fp32 feed_forward*/linear2 nodes: {len(summary['remaining_fp32_linear2'])}")
    in_mb = summary["in_size_bytes"] / 1e6
    out_mb = summary["out_size_bytes"] / 1e6
    pct = 100 * summary["out_size_bytes"] / summary["in_size_bytes"]
    print(f"size: {in_mb:.1f} MB -> {out_mb:.1f} MB ({pct:.1f}%)")


if __name__ == "__main__":
    main()
