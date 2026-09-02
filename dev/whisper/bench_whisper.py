#!/usr/bin/env python3
"""Whisper stage-timeline benchmark harness (whisper perf WP01).

    cd server && uv run --extra whisper python ../dev/whisper/bench_whisper.py \
        ../corpus/real/audio/librispeech-1272-128104-0000.wav \
        --model tiny --reps 10 --json ../results/whisper.jsonl

    # replay a saved run's summary without re-measuring anything:
    uv run --extra whisper python ../dev/whisper/bench_whisper.py \
        --replay ../results/whisper.jsonl

Times four spans of the shipped batch decode - ``features`` (log-mel
extraction), ``encode`` (all N encoder calls summed), ``decode`` (all N
``generate_with_fallback`` calls, temperature ladder included) and ``other``
(the residue: tokenizer work, timestamp alignment, generator plumbing) - by
patching those three methods on faster-whisper's own classes for the duration
of the run, so the numbers come from the real ``model.transcribe`` call rather
than from a re-implementation of it that drifts. Decode parameters come from
``myna.testbed.whisper.batch_decode_options``, which the adapter itself calls,
for the same reason.

The interesting column is ``mel_frames``. Whisper's encoder input is padded to
a fixed 3000 frames (30 s) per call, in ``faster_whisper.audio.pad_or_trim``,
whatever the audio actually was. If that shows 3000 for a 6 s clip then encode
is a per-call constant and not a per-second rate, which is the premise the
whole streaming half of this rests on (docs/project-plan.md T83).
So it is measured every run rather than assumed.

Every run appends one JSON record to ``--json PATH`` (JSONL - one line per
invocation) with the raw per-rep timings, not just the summary, so new metrics
can be computed over old runs later without re-measuring. The first rep is
warmup: timed, but excluded from the printed stats.

Every record carries the **achieved clock** during its measured region, not
just the governor. A laptop under sustained load does not hold its boost: the
same configuration measured 201 ms on a cool machine and 272 ms an hour later,
at 3.4% CV both times, because the cores had sagged from ~4.9 GHz to ~4.0. A
tight CV proves a run was internally steady and says nothing about whether the
machine is where it was yesterday, so **compare within one sweep, with the
control re-measured alongside**, and use ``cpu_mhz`` to audit anything else.

Reps must agree on the transcript. A decode that is not reproducible makes
every latency comparison meaningless, so a mismatch is reported loudly rather
than averaged over. Dispersion is checked the same way: a single warmup rep
does NOT absorb a cold start here (measured 2026-09-02 - a first invocation
after the model file left the page cache printed a median 3.5x the warm one at
17.5% CV, which reads exactly like a real regression), so a run whose total CV
exceeds ``MAX_TOTAL_CV_PCT`` says so instead of quietly reporting the mean of a
warming machine. Raise ``--warmup`` and re-run rather than believing it.

Before measuring, this calls ``bench_guard.check()`` with the ``whisper``
profile. A hard violation refuses to run at all unless ``--force`` is passed;
major page faults sampled around the measured region are checked the same way
after the run.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import statistics
import sys
import threading
import time
import wave
from datetime import UTC, datetime
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives there

import bench_guard  # noqa: E402
from myna.testbed.whisper import WHISPER_RATE, batch_decode_options  # noqa: E402

STAGES = ("features", "encode", "decode", "other")

# Above this, the run is not steady state and its median is not a measurement.
# The warm runs behind the 2026-09-02 baseline sit at 0.9-5.9% total CV; the
# cold one that motivated this check was 17.5%.
MAX_TOTAL_CV_PCT = 10.0

# Below this share of the nominal max clock, the machine is not where a
# previous session's numbers were taken and cross-run comparison is unsafe.
MIN_CLOCK_FRACTION = 0.85
_CLOCK_SAMPLE_SECONDS = 0.1


class _ClockSampler:
    """Median achieved clock over the measured region, sampled from a side
    thread because the measuring thread is busy being measured."""

    def __init__(self) -> None:
        self.samples: list[float] = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self._stop.wait(_CLOCK_SAMPLE_SECONDS):
            mhz = bench_guard.sample_cpu_mhz()
            if mhz:
                self.samples.append(mhz)

    def __enter__(self) -> _ClockSampler:
        self._thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self._stop.set()
        self._thread.join(timeout=1.0)

    @property
    def median(self) -> float | None:
        return statistics.median(self.samples) if self.samples else None


PROFILE = bench_guard.PROFILES["whisper"]

# Whisper's encoder context, in mel frames. Not a tunable here - it is what
# faster_whisper.audio.pad_or_trim pads every segment to, and the number this
# harness exists to confirm is still true of the version installed.
WHISPER_MEL_FRAMES = 3000


def _load_wav(path: Path) -> tuple[np.ndarray, float]:
    with wave.open(str(path)) as w:
        if w.getframerate() != WHISPER_RATE or w.getnchannels() != 1 or w.getsampwidth() != 2:
            raise SystemExit(
                f"{path}: need {WHISPER_RATE} Hz mono S16LE, got {w.getframerate()} Hz "
                f"{w.getnchannels()}ch {8 * w.getsampwidth()}-bit"
            )
        pcm = w.readframes(w.getnframes())
    # The same conversion the adapter's _transcribe does, so the measured
    # input is bit-identical to the shipped one.
    samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
    return samples, len(samples) / WHISPER_RATE


@contextlib.contextmanager
def _instrumented(acc: dict):
    """Time faster-whisper's three compute stages in place.

    Patches the classes, not the instance: ``FeatureExtractor.__call__`` is a
    dunder, and Python resolves those on the type, so an instance attribute
    would simply be ignored. Restored on the way out, including on an
    exception, so a failed rep cannot leave the library instrumented for
    whatever runs next in the same interpreter.
    """
    from faster_whisper.feature_extractor import FeatureExtractor
    from faster_whisper.transcribe import WhisperModel

    originals = {
        (FeatureExtractor, "__call__"): FeatureExtractor.__call__,
        (WhisperModel, "encode"): WhisperModel.encode,
        (WhisperModel, "generate_with_fallback"): WhisperModel.generate_with_fallback,
    }

    def timed_features(self, *a, **kw):
        t0 = time.perf_counter()
        out = originals[(FeatureExtractor, "__call__")](self, *a, **kw)
        acc["features"] += time.perf_counter() - t0
        return out

    def timed_encode(self, features, *a, **kw):
        t0 = time.perf_counter()
        out = originals[(WhisperModel, "encode")](self, features, *a, **kw)
        acc["encode"] += time.perf_counter() - t0
        acc["encode_calls"] += 1
        # features is (n_mels, T) or (batch, n_mels, T); T is the padded axis.
        acc["mel_frames"].append(int(np.shape(features)[-1]))
        return out

    def timed_decode(self, *a, **kw):
        t0 = time.perf_counter()
        out = originals[(WhisperModel, "generate_with_fallback")](self, *a, **kw)
        acc["decode"] += time.perf_counter() - t0
        acc["decode_calls"] += 1
        # (result, avg_logprob, temperature, compression_ratio). A non-zero
        # temperature means the fallback ladder fired on this segment, which
        # is a tail-latency event worth seeing rather than averaging away.
        with contextlib.suppress(IndexError, TypeError):
            acc["temperatures"].append(float(out[2]))
        return out

    FeatureExtractor.__call__ = timed_features
    WhisperModel.encode = timed_encode
    WhisperModel.generate_with_fallback = timed_decode
    try:
        yield
    finally:
        for (cls, name), fn in originals.items():
            setattr(cls, name, fn)


def _one_rep(model, samples: np.ndarray, language: str | None) -> dict:
    acc = {
        "features": 0.0,
        "encode": 0.0,
        "decode": 0.0,
        "encode_calls": 0,
        "decode_calls": 0,
        "mel_frames": [],
        "temperatures": [],
    }
    t0 = time.perf_counter()
    with _instrumented(acc):
        segments, _info = model.transcribe(samples, **batch_decode_options(language, None))
        text = "".join(s.text for s in segments)  # drains the generator: all work happens here
    total = time.perf_counter() - t0

    rep = {stage: 0.0 for stage in STAGES}
    for stage in ("features", "encode", "decode"):
        rep[stage] = acc[stage] * 1000
    rep["other"] = total * 1000 - rep["features"] - rep["encode"] - rep["decode"]
    rep["total"] = total * 1000
    rep["encode_calls"] = acc["encode_calls"]
    rep["decode_calls"] = acc["decode_calls"]
    rep["mel_frames"] = acc["mel_frames"]
    rep["temperatures"] = acc["temperatures"]
    rep["text"] = text.strip()
    return rep


def run_bench(
    wav: Path,
    reps: int,
    warmup: int,
    model_size: str,
    compute_type: str,
    threads: int | None,
    workers: int,
    language: str | None,
    window: float,
) -> tuple[dict, bench_guard.Violation | None]:
    from faster_whisper import WhisperModel

    samples, duration = _load_wav(wav)
    if window:
        samples = samples[: int(window * WHISPER_RATE)]
        duration = len(samples) / WHISPER_RATE

    # cpu_threads=0 is faster-whisper's own default and means "let CTranslate2
    # decide"; the adapter passes nothing at all, so 0 here is the shipped
    # configuration and any other value is the WP04 experiment.
    t0 = time.perf_counter()
    model = WhisperModel(
        model_size,
        device="cpu",
        compute_type=compute_type,
        cpu_threads=threads or 0,
        num_workers=workers,
    )
    load_ms = (time.perf_counter() - t0) * 1000

    # A cold load legitimately faults in the whole model; only the measured
    # region itself is checked against the page-fault threshold.
    majflt_before = bench_guard.sample_majflt()
    with _ClockSampler() as clock:
        all_reps = [_one_rep(model, samples, language) for _ in range(reps + warmup)]
    majflt_after = bench_guard.sample_majflt()
    page_fault_violation = bench_guard.check_page_faults(majflt_before, majflt_after)

    warmup_reps, kept = all_reps[:warmup], all_reps[warmup:]
    texts = {r["text"] for r in kept}

    record = {
        "started_at": datetime.now(UTC).isoformat(),
        "wav": str(wav),
        "audio_duration_seconds": duration,
        "window_seconds": window or None,
        "model": model_size,
        "compute_type": compute_type,
        "cpu_threads": threads or 0,
        "num_workers": workers,
        "language": language,
        "load_ms": load_ms,
        "faster_whisper": _version("faster_whisper"),
        "ctranslate2": _version("ctranslate2"),
        "cpu_mhz": clock.median,
        "cpu_max_mhz": bench_guard.cpu_max_mhz(),
        "reproducible": len(texts) == 1,
        "text": kept[0]["text"],
        "warmup_total_ms": [r["total"] for r in warmup_reps],
        "reps": kept,
    }
    return record, page_fault_violation


def _version(module: str) -> str:
    try:
        return __import__(module).__version__
    except (ImportError, AttributeError):
        return "unknown"


def _stat(values: list[float]) -> dict[str, float]:
    mean = statistics.fmean(values)
    stdev = statistics.stdev(values) if len(values) > 1 else 0.0
    return {
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
        "mean": mean,
        "cv_pct": 100 * stdev / mean if mean else 0.0,
    }


def print_summary(record: dict) -> None:
    reps = record["reps"]
    duration = record["audio_duration_seconds"]
    stats = {k: _stat([r[k] for r in reps]) for k in (*STAGES, "total")}
    meta = reps[0]

    mel = meta["mel_frames"]
    mel_note = f"{mel[0]}" if len({*mel}) == 1 else f"{min(mel)}-{max(mel)}"
    print(
        f"{record['model']}/{record['compute_type']}  audio {duration:.2f}s  "
        f"encode_calls {meta['encode_calls']}  decode_calls {meta['decode_calls']}  "
        f"mel_frames {mel_note}  threads {record['cpu_threads'] or 'ct2-default'}  "
        f"reps {len(reps)}  load {record['load_ms']:.0f}ms"
    )
    if not record.get("reproducible", True):
        print("WARNING: reps disagreed on the transcript; timings are not comparable")
    mhz, mhz_max = record.get("cpu_mhz"), record.get("cpu_max_mhz")
    if mhz and mhz_max:
        print(f"clock during run: {mhz:.0f} MHz of {mhz_max:.0f} nominal ({mhz / mhz_max:.0%})")
        if mhz < MIN_CLOCK_FRACTION * mhz_max:
            print(
                f"WARNING: cores held only {mhz / mhz_max:.0%} of nominal clock. The run may "
                "be internally steady and still not comparable to one taken on a cooler "
                "machine - a 35% swing has been measured this way. Compare within a sweep."
            )
    total_cv = stats["total"]["cv_pct"]
    if total_cv > MAX_TOTAL_CV_PCT:
        print(
            f"WARNING: total CV {total_cv:.1f}% is over {MAX_TOTAL_CV_PCT:.0f}% - this run is "
            "not steady state (cold page cache, a competing process, or a ramping clock). "
            "Raise --warmup and re-run; do not compare this median to anything."
        )
    ladder = [t for r in reps for t in r["temperatures"] if t]
    if ladder:
        print(f"WARNING: temperature fallback fired ({len(ladder)} segment(s) above T=0)")

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
    if meta["encode_calls"]:
        per_call = stats["encode"]["median"] / meta["encode_calls"]
        padded = mel and min(mel) == WHISPER_MEL_FRAMES
        note = (
            f" (a fixed {WHISPER_MEL_FRAMES}-frame / 30 s context: this cost does not "
            "shrink with the window)"
            if padded
            else ""
        )
        print(f"encode per call: {per_call:.1f} ms{note}")
    print(f"text: {record['text'][:110]}")


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("wav", nargs="?", type=Path, help="16 kHz mono PCM WAV clip")
    ap.add_argument("--reps", type=int, default=10, help="measured decode repetitions")
    ap.add_argument(
        "--warmup",
        type=int,
        default=2,
        help="unmeasured repetitions run first (default 2; one does not absorb a cold start)",
    )
    ap.add_argument(
        "--model",
        default="tiny",
        help="model size name or a CTranslate2 model directory (default: tiny)",
    )
    ap.add_argument(
        "--compute-type",
        default=None,
        help="CTranslate2 arithmetic (default: the shipped per-model choice, "
        "int8 for tiny and float32 for base/small)",
    )
    ap.add_argument(
        "--threads",
        type=int,
        default=None,
        help="cpu_threads (default: unset, which is what the adapter ships - WP04)",
    )
    ap.add_argument("--workers", type=int, default=1, help="num_workers / CT2 inter_threads")
    ap.add_argument("--language", default="en", help="decode language (default: en)")
    ap.add_argument(
        "--window", type=float, default=0.0, help="trim audio to N seconds (0 = full clip)"
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
        help='measure and write a record even with guard violations, stamping "dirty" into it',
    )
    ap.add_argument(
        "--replay",
        type=Path,
        default=None,
        help="print the summary for the last record in this JSON(L) file, no measuring",
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

    # Mirrors models/*/model.yaml's MODEL_COMPUTE_TYPE, which is where the
    # 2026-08-26 measurement lives (project-plan T70). A model directory or an
    # unrecognised name has no shipped default, so it takes CTranslate2's.
    compute_type = args.compute_type or {"tiny": "int8"}.get(args.model, "float32")

    pre_violations = bench_guard.check(PROFILE)
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

    record, page_fault_violation = run_bench(
        args.wav,
        args.reps,
        args.warmup,
        args.model,
        compute_type,
        args.threads,
        args.workers,
        args.language,
        args.window,
    )
    if page_fault_violation:
        print(page_fault_violation, file=sys.stderr)

    dirty = hard_pre + ([page_fault_violation] if page_fault_violation else [])
    if dirty:
        record["environment"] = "dirty"
        record["guard_violations"] = [str(v) for v in dirty]

    print_summary(record)

    if args.json:
        if dirty and not args.force:
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
