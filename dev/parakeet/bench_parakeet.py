#!/usr/bin/env python3
"""Parakeet stage-timeline benchmark harness (perf T01).

    cd server && uv run python ../dev/parakeet/bench_parakeet.py \
        ../corpus/english/audio/librispeech-422-122949-0001.wav \
        --reps 15 --threads 4 --json ../results/result.json

    # replay a saved run's summary without re-measuring anything:
    uv run python ../dev/parakeet/bench_parakeet.py \
        --replay ../results/result.json

    # per-operator encoder breakdown (ORT's own node profiler):
    uv run python ../dev/parakeet/bench_parakeet.py ../corpus/english/audio/foo.wav \
        --profile-nodes /tmp/encprof

Times the same five spans as the 2026-08-28 baseline by calling
``myna.testbed.parakeet._ParakeetOnnx`` directly through its ``bench`` hook,
so this can never drift from what the adapter ships: ``preprocess``,
``encode``, ``transpose``, ``joint`` (all N decoder_joint calls summed) and
``greedy`` (the decode loop's own argmax/control-flow overhead, isolated by
subtracting summed joint time from loop wall time).

Every run appends one JSON record to ``--json PATH`` (JSONL — one line per
invocation) with the raw per-rep timings, not just the summary, so new
metrics can be computed over old runs later without re-measuring. The first
rep is warmup: timed, but excluded from the printed/summarized stats.

Before measuring, this calls ``bench_guard.check()``. A hard violation
refuses to run at all unless ``--force`` is passed; major page faults
sampled around the measured region are checked the same way after the run,
since that check is necessarily post-hoc. Either way, a forced-through
violation stamps ``"environment": "dirty"`` into the record so a
contaminated number can never be mistaken for a clean one later.
"""

from __future__ import annotations

import argparse
import collections
import json
import statistics
import sys
import time
import wave
from datetime import UTC, datetime
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives there

import bench_guard  # noqa: E402
from myna.testbed.parakeet import (  # noqa: E402
    PARAKEET_RATE,
    _default_model_dir,
    _encoder_threads,
    _ParakeetOnnx,
)

STAGES = ("preprocess", "encode", "transpose", "joint", "greedy")
COUNT_KEYS = ("_frames", "_joint_calls")


def _load_wav(path: Path) -> tuple[np.ndarray, float]:
    with wave.open(str(path), "rb") as wav:
        if wav.getcomptype() != "NONE":
            raise ValueError(f"{path}: only uncompressed PCM WAV is supported")
        if wav.getnchannels() != 1 or wav.getframerate() != PARAKEET_RATE:
            raise ValueError(
                f"{path}: need {PARAKEET_RATE} Hz mono, got "
                f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
            )
        pcm = np.frombuffer(wav.readframes(wav.getnframes()), dtype=np.int16)
    samples = pcm.astype(np.float32) / 32768.0
    return samples, len(samples) / PARAKEET_RATE


def _one_rep(model: _ParakeetOnnx, samples: np.ndarray) -> dict:
    """Run one decode, returning stage times in ms plus the counts, keyed
    exactly as ``STAGES`` + ``"total"`` + ``COUNT_KEYS``."""
    raw: dict[str, float] = {}

    def bench(name: str, value: float) -> None:
        raw[name] = raw.get(name, 0.0) + value

    model.transcribe(samples, bench=bench)
    rep = {stage: raw.get(stage, 0.0) * 1000 for stage in STAGES}  # s -> ms
    rep["total"] = sum(rep.values())
    for key in COUNT_KEYS:
        rep[key] = raw.get(key, 0.0)
    return rep


def run_bench(
    wav: Path, reps: int, threads: int, window: float, model_dir: str
) -> tuple[dict, bench_guard.Violation | None]:
    samples, duration = _load_wav(wav)
    if window:
        samples = samples[: int(window * PARAKEET_RATE)]
        duration = len(samples) / PARAKEET_RATE

    t0 = time.perf_counter()
    model = _ParakeetOnnx(model_dir, encoder_threads=threads)
    load_ms = (time.perf_counter() - t0) * 1000

    # Model load legitimately faults in ~794 MB the first time; only the
    # measured region itself is checked against the T02 page-fault threshold.
    majflt_before = bench_guard.sample_majflt()
    all_reps = [_one_rep(model, samples) for _ in range(reps + 1)]
    majflt_after = bench_guard.sample_majflt()
    page_fault_violation = bench_guard.check_page_faults(majflt_before, majflt_after)

    kept = all_reps[1:]  # discard warmup
    meta = kept[0]

    record = {
        "started_at": datetime.now(UTC).isoformat(),
        "wav": str(wav),
        "audio_duration_seconds": duration,
        "window_seconds": window or None,
        "threads": threads,
        "model_dir": str(model_dir),
        "load_ms": load_ms,
        "frames": int(meta["_frames"]),
        "joint_calls": int(meta["_joint_calls"]),
        "reps": kept,
    }
    return record, page_fault_violation


def _stat(values: list[float]) -> dict[str, float]:
    mean = statistics.fmean(values)
    stdev = statistics.pstdev(values) if len(values) > 1 else 0.0
    return {
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
        "cv_pct": (100 * stdev / mean) if mean else 0.0,
    }


def print_summary(record: dict) -> None:
    reps = record["reps"]
    duration = record["audio_duration_seconds"]
    keys = (*STAGES, "total")
    stats = {k: _stat([r[k] for r in reps]) for k in keys}

    print(
        f"audio {duration:.2f}s  frames {record['frames']}  "
        f"joint_calls {record['joint_calls']}  threads {record['threads']}  "
        f"reps {len(reps)}  load {record['load_ms']:.0f}ms"
    )
    print(f"{'stage':<12}{'min':>9}{'med':>9}{'max':>9}{'CV%':>7}{'%tot':>7}{'ms/s':>8}")
    total_med = stats["total"]["median"]
    for k in STAGES:
        s = stats[k]
        print(
            f"{k:<12}{s['min']:>9.2f}{s['median']:>9.2f}{s['max']:>9.2f}{s['cv_pct']:>7.1f}"
            f"{100 * s['median'] / total_med:>7.1f}{s['median'] / duration:>8.2f}"
        )
    s = stats["total"]
    print(
        f"{'TOTAL':<12}{s['min']:>9.2f}{s['median']:>9.2f}{s['max']:>9.2f}{s['cv_pct']:>7.1f}"
        f"{100:>7.1f}{s['median'] / duration:>8.2f}   "
        f"RTF med {s['median'] / 1000 / duration:.4f} best {s['min'] / 1000 / duration:.4f}"
    )
    if record["joint_calls"]:
        print(f"joint per call: {stats['joint']['median'] / record['joint_calls'] * 1000:.0f} us")


def profile_nodes(
    model_dir: str, wav: Path, window: float, threads: int, out_dir: Path, reps: int
) -> None:
    """ORT's own node profiler over the encoder: per-op_name kernel time,
    aggregated from the *last* run's nodes only. A profiling session appends
    one event per graph node per ``.run()`` call, so each node ``name``
    recurs once per rep — summing them all (or grouping by ``args.run_index``,
    which onnxruntime 1.27 never sets — verified empirically, every event's
    run_index is absent) silently multiplies the total by the rep count.
    Keeping only the event with the latest ``ts`` for each name isolates the
    steady-state last run without that inflation."""
    import onnxruntime as ort

    samples, _ = _load_wav(wav)
    if window:
        samples = samples[: int(window * PARAKEET_RATE)]

    model = _ParakeetOnnx(model_dir, encoder_threads=threads)
    waveforms = samples.reshape(1, -1).astype(np.float32)
    waveforms_lens = np.array([samples.shape[0]], dtype=np.int64)
    features, features_lens = model._preprocessor.run(
        ["features", "features_lens"], {"waveforms": waveforms, "waveforms_lens": waveforms_lens}
    )

    opts = ort.SessionOptions()
    opts.log_severity_level = 3
    opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    opts.intra_op_num_threads = threads
    opts.inter_op_num_threads = 1
    opts.enable_profiling = True
    # Profile the same encoder variant the adapter would run (base or
    # maxstack + custom-op library) so the two instruments never diverge on
    # *what* they measure — methodology rule 7.
    from myna.testbed.parakeet import encoder_variant

    encoder_path, custom_ops = encoder_variant(model_dir)
    if custom_ops:
        opts.register_custom_ops_library(custom_ops)
    out_dir.mkdir(parents=True, exist_ok=True)
    opts.profile_file_prefix = str(out_dir / "encprof")
    enc = ort.InferenceSession(encoder_path, opts, providers=["CPUExecutionProvider"])
    for _ in range(reps):
        enc.run(["outputs", "encoded_lengths"], {"audio_signal": features, "length": features_lens})
    trace_path = enc.end_profiling()
    print(f"profile: {trace_path}")

    events = json.loads(Path(trace_path).read_text(encoding="utf-8"))
    nodes = [e for e in events if e.get("cat") == "Node" and e["name"].endswith("_kernel_time")]
    last_by_name: dict[str, dict] = {}
    for e in nodes:
        prior = last_by_name.get(e["name"])
        if prior is None or e["ts"] > prior["ts"]:
            last_by_name[e["name"]] = e
    last = list(last_by_name.values())

    by_op: collections.Counter = collections.Counter()
    count: collections.Counter = collections.Counter()
    for e in last:
        op = e["args"].get("op_name", "?")
        by_op[op] += e["dur"]
        count[op] += 1
    total_us = sum(by_op.values()) or 1

    print(
        f"\nencoder nodes={len(last)} kernel_total={total_us / 1000:.1f} ms  "
        f"(features T={features.shape[-1]})"
    )
    print(f"{'op_type':<28}{'ms':>9}{'%':>7}{'count':>7}{'us/call':>9}")
    for op, us in by_op.most_common(20):
        print(
            f"{op:<28}{us / 1000:>9.2f}{100 * us / total_us:>7.1f}"
            f"{count[op]:>7}{us / count[op]:>9.1f}"
        )


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("wav", nargs="?", type=Path, help="16 kHz mono PCM WAV clip")
    ap.add_argument(
        "--reps", type=int, default=15, help="decode repetitions (first is discarded as warmup)"
    )
    ap.add_argument(
        "--threads",
        type=int,
        default=None,
        help="encoder intra-op threads (default: production heuristic)",
    )
    ap.add_argument(
        "--window", type=float, default=0.0, help="trim audio to N seconds (0 = full clip)"
    )
    ap.add_argument(
        "--model", type=str, default=None, help="model dir (default: staged parakeet weights)"
    )
    ap.add_argument(
        "--json",
        type=Path,
        default=None,
        help="append the run's raw+summary record as one JSON line",
    )
    ap.add_argument(
        "--force",
        action="store_true",
        help=(
            "measure and write a record even with T02 guard violations, "
            'stamping "environment": "dirty" into it'
        ),
    )
    ap.add_argument(
        "--replay",
        type=Path,
        default=None,
        help="print the summary for the last record in this JSON(L) file, no measuring",
    )
    ap.add_argument(
        "--profile-nodes",
        type=Path,
        default=None,
        metavar="DIR",
        help="ORT node profiler mode: aggregate encoder op time, trace written under DIR",
    )
    args = ap.parse_args()

    if args.replay is not None:
        lines = [
            line for line in args.replay.read_text(encoding="utf-8").splitlines() if line.strip()
        ]
        if not lines:
            raise SystemExit(f"{args.replay}: no records")
        print_summary(json.loads(lines[-1]))
        return

    if args.wav is None:
        ap.error("wav is required unless --replay is given")

    model_dir = args.model or _default_model_dir()
    threads = args.threads if args.threads is not None else _encoder_threads()

    if args.profile_nodes is not None:
        profile_nodes(
            model_dir, args.wav, args.window, threads, args.profile_nodes, max(args.reps, 3)
        )
        return

    pre_violations = bench_guard.check(bench_guard.PROFILES["parakeet"])
    for v in pre_violations:
        print(v, file=sys.stderr)
    hard_pre = [v for v in pre_violations if v.severity == bench_guard.HARD]
    if hard_pre and not args.force:
        print(
            "refusing to measure on a contaminated machine (see violations above); "
            "fix the environment or pass --force to record it anyway as dirty",
            file=sys.stderr,
        )
        raise SystemExit(1)

    record, page_fault_violation = run_bench(args.wav, args.reps, threads, args.window, model_dir)
    if page_fault_violation:
        print(page_fault_violation, file=sys.stderr)

    dirty_violations = hard_pre + ([page_fault_violation] if page_fault_violation else [])
    if dirty_violations:
        record["environment"] = "dirty"
        record["guard_violations"] = [str(v) for v in dirty_violations]

    print_summary(record)

    if args.json:
        if dirty_violations and not args.force:
            print(
                "refusing to write a dirty record without --force (see violations above)",
                file=sys.stderr,
            )
            raise SystemExit(1)
        args.json.parent.mkdir(parents=True, exist_ok=True)
        with args.json.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(record) + "\n")


if __name__ == "__main__":
    main()
