#!/usr/bin/env python3
"""Streaming duty-cycle telemetry (perf T03).

    cd server && uv run python ../dev/parakeet/bench_streaming.py \
        ../corpus/real/audio/librispeech-3081-166546-longform.wav \
        --window 60 --json ../results/result.json

    # replay a saved run's summary without re-measuring anything:
    uv run python ../dev/parakeet/bench_streaming.py \
        --replay ../results/result.json

Drives a real ``ParakeetAdapter`` streaming session (the shipped code path:
``run_session`` -> model load -> ``_run_streaming_session`` ->
``run_streaming_loop``) over a WAV clip fed at real-time pace, via
``LoopbackClient`` so the adapter runs in-process and a
``myna.testbed.harness.StreamingTelemetry`` built by this script can be
threaded into both the adapter's constructor and ``Harness.run`` - the only
way to get this number out, since it is invisible on the wire (see
``StreamingTelemetry``'s docstring). Real-time pacing matters: ``duty_cycle``
divides by wall-clock session time, so a batch-fed (as-fast-as-possible)
session would show ~100% instead of a live dictation session's true duty.

Prints ``decode_calls`` by kind, ``audio_seconds_ingested`` (session wall
audio), ``audio_seconds_encoded`` (summed decode window, RollingWindow
overlap included -- the quantity behind the streaming duty-cycle
multiplier), ``encoder_busy_seconds``, the derived ``redundancy`` and
``duty_cycle``, and window-length min/median/max.

Before measuring, this calls ``bench_guard.check()`` the same way
dev/parakeet/bench_parakeet.py does.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
import wave
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

import bench_guard  # noqa: E402

from myna.core import AudioFormat, LoopbackClient, PcmChunk, SessionConfig  # noqa: E402
from myna.testbed.harness import Harness, StreamingTelemetry  # noqa: E402
from myna.testbed.parakeet import (  # noqa: E402
    PARAKEET_RATE,
    PARTIAL_CADENCE_S,
    PARTIAL_TAIL_S,
    ParakeetAdapter,
    _default_model_dir,
)
from myna.testbed.streaming.strategies import SC_ARM_S  # noqa: E402


class _RealtimeClip:
    """A WAV file, optionally trimmed to its first ``window`` seconds,
    streamed at real-time pace -- ``myna.testbed.sources.WavFileSource`` has
    no trim, and pacing here must always be real-time (see module docstring),
    so this is its own small ``AudioSource``."""

    def __init__(self, path: Path, window: float = 0.0, chunk_seconds: float = 0.1) -> None:
        self._path = Path(path)
        self._window = window
        self._chunk_seconds = chunk_seconds
        with wave.open(str(self._path), "rb") as wav:
            if wav.getcomptype() != "NONE":
                raise ValueError(f"{self._path}: only uncompressed PCM WAV is supported")
            if wav.getnchannels() != 1 or wav.getframerate() != PARAKEET_RATE:
                raise ValueError(
                    f"{self._path}: need {PARAKEET_RATE} Hz mono, got "
                    f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
                )
            self._format = AudioFormat(
                sample_rate_hz=wav.getframerate(), channels=1, sample_width_bytes=wav.getsampwidth()
            )

    @property
    def format(self) -> AudioFormat:
        return self._format

    async def chunks(self):
        frames_per_chunk = max(1, round(self._format.sample_rate_hz * self._chunk_seconds))
        with wave.open(str(self._path), "rb") as wav:
            total = (
                int(self._window * self._format.sample_rate_hz)
                if self._window
                else wav.getnframes()
            )
            sent = 0
            while sent < total:
                n = min(frames_per_chunk, total - sent)
                data = wav.readframes(n)
                if not data:
                    break
                sent += n
                chunk = PcmChunk(data=data, format=self._format)
                await asyncio.sleep(chunk.duration_seconds)
                yield chunk


def _print_summary(summary: dict, audio_duration: float) -> None:
    print(
        f"audio_duration {audio_duration:.2f}s  session_seconds {summary['session_seconds']:.2f}s"
    )
    print(f"decode_calls {summary['decode_calls']}")
    print(
        f"audio_seconds_ingested {summary['audio_seconds_ingested']:.2f}  "
        f"audio_seconds_encoded {summary['audio_seconds_encoded']:.2f}  "
        f"encoder_busy_seconds {summary['encoder_busy_seconds']:.3f}"
    )
    redundancy = summary["redundancy"]
    duty_cycle = summary["duty_cycle"]
    redundancy_str = f"{redundancy:.2f}x" if redundancy is not None else "n/a"
    duty_cycle_str = f"{100 * duty_cycle:.1f}%" if duty_cycle is not None else "n/a"
    print(f"redundancy {redundancy_str}   duty_cycle {duty_cycle_str}")
    window_stats = summary["window_seconds"]
    if window_stats:
        print(
            f"window_seconds min {window_stats['min']:.2f}  "
            f"median {window_stats['median']:.2f}  max {window_stats['max']:.2f}"
        )


async def run_bench(
    wav: Path,
    window: float,
    cadence: float,
    tail: float,
    arm: float,
    model_dir: str,
) -> tuple[dict, bench_guard.Violation | None]:
    telemetry = StreamingTelemetry()
    adapter = ParakeetAdapter(
        model_dir,
        streaming=True,
        stream_arm_s=arm,
        stream_partial_cadence_s=cadence,
        stream_partial_tail_s=tail,
        stream_telemetry=telemetry,
    )
    # Warm the model before sampling major faults, the same way
    # dev/parakeet/bench_parakeet.py constructs its model before sampling: loading
    # legitimately faults in ~794 MB once and that cost isn't what T02 checks.
    await adapter._load_model()  # noqa: SLF001 -- dev tooling, same precedent as T01's _ParakeetOnnx reuse

    source = _RealtimeClip(wav, window=window)
    majflt_before = bench_guard.sample_majflt()
    t0 = time.perf_counter()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=source,
        config=SessionConfig(audio_format=source.format),
        streaming_telemetry=telemetry,
    )
    wall_s = time.perf_counter() - t0
    majflt_after = bench_guard.sample_majflt()
    page_fault_violation = bench_guard.check_page_faults(majflt_before, majflt_after)

    out = {
        "started_at": datetime.now(UTC).isoformat(),
        "wav": str(wav),
        "window_seconds": window or None,
        "cadence_seconds": cadence,
        "tail_seconds": tail,
        "arm_seconds": arm,
        "model_dir": str(model_dir),
        "wall_seconds": wall_s,
        "transcript": record.transcript,
        "streaming_telemetry": record.to_json()["streaming_telemetry"],
    }
    return out, page_fault_violation


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("wav", nargs="?", type=Path, help="16 kHz mono PCM WAV clip")
    ap.add_argument(
        "--window", type=float, default=0.0, help="trim to the first N seconds (0 = whole clip)"
    )
    ap.add_argument(
        "--cadence",
        type=float,
        default=PARTIAL_CADENCE_S,
        help="--stream-partial-cadence-s (shipped default 2.0, perf T04)",
    )
    ap.add_argument(
        "--tail",
        type=float,
        default=PARTIAL_TAIL_S,
        help="--stream-partial-tail-s (shipped default 0 = whole uncommitted window)",
    )
    ap.add_argument(
        "--arm", type=float, default=SC_ARM_S, help="--stream-arm-s (shipped default 15)"
    )
    ap.add_argument(
        "--model", type=str, default=None, help="model dir (default: staged parakeet weights)"
    )
    ap.add_argument(
        "--json",
        type=Path,
        default=None,
        help="append the run's record (raw samples + summary) as one JSON line",
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
    args = ap.parse_args()

    if args.replay is not None:
        lines = [
            line for line in args.replay.read_text(encoding="utf-8").splitlines() if line.strip()
        ]
        if not lines:
            raise SystemExit(f"{args.replay}: no records")
        rec = json.loads(lines[-1])
        telemetry = rec["streaming_telemetry"]
        _print_summary(telemetry["summary"], telemetry["audio_seconds_ingested"])
        return

    if args.wav is None:
        ap.error("wav is required unless --replay is given")

    model_dir = args.model or _default_model_dir()

    pre_violations = bench_guard.check()
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

    record, page_fault_violation = asyncio.run(
        run_bench(args.wav, args.window, args.cadence, args.tail, args.arm, model_dir)
    )
    if page_fault_violation:
        print(page_fault_violation, file=sys.stderr)

    dirty_violations = hard_pre + ([page_fault_violation] if page_fault_violation else [])
    if dirty_violations:
        record["environment"] = "dirty"
        record["guard_violations"] = [str(v) for v in dirty_violations]

    telemetry = record["streaming_telemetry"]
    _print_summary(telemetry["summary"], telemetry["audio_seconds_ingested"])

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
