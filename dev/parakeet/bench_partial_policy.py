#!/usr/bin/env python3
"""Partial-policy trade surface sweep (perf T04).

    cd server && uv run python ../dev/parakeet/bench_partial_policy.py \\
        --clip synthetic:/tmp/synthetic_paced_dictation.wav \\
        --cadence 0.5,1.0,2.0 --tail 0,3,5,8 \\
        --json ../results/result.json

    # replay a saved sweep's table without re-measuring anything:
    uv run python ../dev/parakeet/bench_partial_policy.py --replay result.json

Maps display quality against encoder cost across the
``(stream_partial_cadence_s, stream_partial_tail_s)`` grid (perf T04). Reuses
T03's ``StreamingTelemetry`` for the cost side (``duty_cycle``, ``redundancy``,
window-length distribution) and drives the same real ``ParakeetAdapter``
streaming session real-time-paced via ``LoopbackClient``
(``dev/parakeet/bench_streaming.py``'s approach) — but also keeps the session's raw
wire events, which T03 discarded, because the *display*-quality metrics this
task needs (unstable-text churn, head loss, staleness) are only visible
there.

Quality metrics, computed from the unstable ``TranscriptionFinal`` event
sequence (see module docstring section "Metric definitions" below for the
exact algorithm — written down so a rerun months later is comparable):

- ``time_to_first_unstable`` — onset latency (existing ``harness.Metrics``
  field, reused as-is).
- ``staleness_s`` — median/p90 wall-clock cost of a "partial" decode call
  (``StreamingTelemetry`` per-sample ``wall_seconds``). This *is* the lag: the
  loop cannot show text for audio it hasn't finished encoding, so a tick's own
  compute time is exactly how far behind the frontier its result lands.
- ``churn_rate`` — fraction of consecutive unstable-text updates, within one
  commit epoch, that are not a pure prefix-extension of the previous update
  (i.e. some previously-shown word changed rather than just growing).
- ``head_loss_rate`` — fraction of those same transitions where the previous
  update's *first* word is entirely absent from the new update — the
  structural signature of a tail cap dropping the window's head, distinct
  from ordinary mid-stream self-correction (which usually only touches the
  last word or two).
- committed-transcript WER, against the clip's reference text (necessary but
  not sufficient per the SPEC — the commit path is chunk-final and shouldn't
  move with partial policy; this is a sanity check that it doesn't).

Joules/minute is NOT re-measured live per cell (a 12-24 cell grid at
real-time pace, each needing the 20 s thermal cooldown T08 found necessary
between RAPL-measured configs, would cost hours). It is derived from T08's
directly-measured marginal encode power on this machine (43.29 W at 4
threads, fast domain, minus 4.90 W idle = 38.39 W marginal while the encoder
is actually running) times this task's measured ``duty_cycle``:
``joules_per_minute = 38.39 * duty_cycle * 60``. This is arithmetic over two
independently-measured quantities, not a fresh measurement, and is reported
as such. A live RAPL spot-check at two grid extremes validates the arithmetic
model (see result.md).

Before measuring, this calls ``bench_guard.check()`` (perf T02), same as
``dev/parakeet/bench_parakeet.py`` and ``dev/parakeet/bench_streaming.py``.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import sys
import time
import wave
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives there

import bench_guard  # noqa: E402
from myna.core import AudioFormat, LoopbackClient, PcmChunk, SessionConfig  # noqa: E402
from myna.core.events import Disposition  # noqa: E402
from myna.testbed.harness import Harness, StreamingTelemetry, compute_metrics  # noqa: E402
from myna.testbed.metrics import normalize, word_error_rate  # noqa: E402
from myna.testbed.parakeet import (  # noqa: E402
    PARAKEET_RATE,
    PARTIAL_CADENCE_S,
    PARTIAL_TAIL_S,
    ParakeetAdapter,
    _default_model_dir,
)
from myna.testbed.streaming.strategies import SC_ARM_S  # noqa: E402

# T08's directly-measured marginal encode power on this machine (fast domain,
# 4 threads): 43.29 W package power minus 4.90 W idle. See module docstring.
_T08_MARGINAL_ENCODE_WATTS = 43.29 - 4.90


class _RealtimeClip:
    """Same as dev/parakeet/bench_streaming.py's — a WAV, optionally trimmed to its
    first ``window`` seconds, streamed at real-time pace."""

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


def _tokens(text: str) -> list[str]:
    norm = normalize(text)
    return norm.split() if norm else []


@dataclass
class QualitySurface:
    """One epoch-aware pass over the unstable-text sequence. See the module
    docstring's "Metric definitions" section for what each field means and
    why. An epoch is the span between two commits (or session start/end) —
    the unstable stream resets (I4) at every commit, so a transition spanning
    a commit is not a real update, it's a fresh start and must not be scored.
    """

    transitions: int = 0
    contradicted: int = 0
    head_lost: int = 0

    @property
    def churn_rate(self) -> float | None:
        return self.contradicted / self.transitions if self.transitions else None

    @property
    def head_loss_rate(self) -> float | None:
        return self.head_lost / self.transitions if self.transitions else None


def _quality_surface(events) -> QualitySurface:
    surface = QualitySurface()
    prev_tokens: list[str] | None = None
    for te in events:
        ev = te.event
        if ev.type != "transcription.final":
            continue
        disposition = getattr(ev, "disposition", Disposition.COMMITTED)
        if disposition == Disposition.COMMITTED:
            prev_tokens = None  # I4: a commit resolves/resets the unstable epoch
            continue
        tokens = _tokens(ev.text)
        if prev_tokens is not None and prev_tokens:
            surface.transitions += 1
            is_extension = tokens[: len(prev_tokens)] == prev_tokens
            if not is_extension:
                surface.contradicted += 1
            if prev_tokens[0] not in tokens:
                surface.head_lost += 1
        prev_tokens = tokens
    return surface


def _staleness(telemetry: StreamingTelemetry, kind: str = "partial") -> dict | None:
    values = sorted(s.wall_seconds for s in telemetry.samples if s.kind == kind)
    if not values:
        return None
    p90_idx = min(len(values) - 1, int(round(0.9 * (len(values) - 1))))
    return {
        "median": statistics.median(values),
        "p90": values[p90_idx],
        "max": values[-1],
    }


async def run_cell(
    wav: Path,
    window: float,
    cadence: float,
    tail: float,
    arm: float,
    model_dir: str,
    reference_text: str | None,
) -> dict:
    telemetry = StreamingTelemetry()
    adapter = ParakeetAdapter(
        model_dir,
        streaming=True,
        stream_arm_s=arm,
        stream_partial_cadence_s=cadence,
        stream_partial_tail_s=tail,
        stream_telemetry=telemetry,
    )
    await adapter._load_model()  # noqa: SLF001 -- dev tooling, same precedent as T03

    source = _RealtimeClip(wav, window=window)
    majflt_before = bench_guard.sample_majflt()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=source,
        config=SessionConfig(audio_format=source.format),
        streaming_telemetry=telemetry,
    )
    majflt_after = bench_guard.sample_majflt()
    page_fault_violation = bench_guard.check_page_faults(majflt_before, majflt_after)

    metrics = compute_metrics(record.events, record.audio_end_t, record.audio_duration_seconds)
    surface = _quality_surface(record.events)
    summary = telemetry.summary()
    duty_cycle = summary["duty_cycle"] or 0.0
    joules_per_minute = _T08_MARGINAL_ENCODE_WATTS * duty_cycle * 60.0

    wer = None
    if reference_text is not None:
        wer = word_error_rate(reference_text, record.transcript).rate

    out = {
        "wav": str(wav),
        "window_seconds": window or None,
        "cadence_seconds": cadence,
        "tail_seconds": tail,
        "arm_seconds": arm,
        "audio_seconds": record.audio_duration_seconds,
        "telemetry": summary,
        "joules_per_minute_derived": joules_per_minute,
        "staleness_s": _staleness(telemetry, "partial"),
        "time_to_first_unstable": metrics.time_to_first_unstable,
        "time_to_first_committed": metrics.time_to_first_committed,
        "churn_rate": surface.churn_rate,
        "head_loss_rate": surface.head_loss_rate,
        "unstable_transitions": surface.transitions,
        "committed_wer": wer,
        "transcript": record.transcript,
    }
    if page_fault_violation:
        out["guard_violation"] = str(page_fault_violation)
    return out


def _print_row(cell: dict) -> None:
    tel = cell["telemetry"]
    duty = tel["duty_cycle"]
    red = tel["redundancy"]
    stale = cell["staleness_s"]
    print(
        f"cadence={cell['cadence_seconds']:<4} tail={cell['tail_seconds']:<4} "
        f"duty={duty * 100:5.1f}% redundancy={red:6.2f}x "
        f"J/min~{cell['joules_per_minute_derived']:5.2f} "
        f"stale_med={stale['median'] * 1000:6.1f}ms stale_p90={stale['p90'] * 1000:6.1f}ms "
        if stale
        else f"cadence={cell['cadence_seconds']:<4} tail={cell['tail_seconds']:<4} "
        f"duty={duty * 100:5.1f}% redundancy={red:6.2f}x "
        f"J/min~{cell['joules_per_minute_derived']:5.2f} stale_med=n/a "
    )
    print(
        f"    churn={cell['churn_rate']}  head_loss={cell['head_loss_rate']}  "
        f"n_transitions={cell['unstable_transitions']}  "
        f"ttf_unstable={cell['time_to_first_unstable']}  wer={cell['committed_wer']}"
    )


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--clip", type=Path, help="16 kHz mono PCM WAV clip")
    ap.add_argument("--window", type=float, default=0.0, help="trim to first N seconds (0=whole)")
    ap.add_argument("--reference-text", type=str, default=None, help="reference for WER scoring")
    ap.add_argument("--cadence", type=str, default=str(PARTIAL_CADENCE_S), help="comma-separated")
    ap.add_argument("--tail", type=str, default=str(PARTIAL_TAIL_S), help="comma-separated")
    ap.add_argument("--arm", type=float, default=SC_ARM_S)
    ap.add_argument("--model", type=str, default=None)
    ap.add_argument("--json", type=Path, default=None, help="append each cell's record as JSON")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--replay", type=Path, default=None, help="print rows from a saved JSONL file")
    args = ap.parse_args()

    if args.replay is not None:
        for line in args.replay.read_text(encoding="utf-8").splitlines():
            if line.strip():
                _print_row(json.loads(line))
        return

    if args.clip is None:
        ap.error("--clip is required unless --replay is given")

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

    model_dir = args.model or _default_model_dir()
    cadences = [float(x) for x in args.cadence.split(",")]
    tails = [float(x) for x in args.tail.split(",")]

    for cadence in cadences:
        for tail in tails:
            t0 = time.perf_counter()
            cell = asyncio.run(
                run_cell(
                    args.clip,
                    args.window,
                    cadence,
                    tail,
                    args.arm,
                    model_dir,
                    args.reference_text,
                )
            )
            cell["measured_at"] = datetime.now(UTC).isoformat()
            cell["wall_seconds"] = time.perf_counter() - t0
            if hard_pre or cell.get("guard_violation"):
                cell["environment"] = "dirty"
            _print_row(cell)
            if args.json:
                if cell.get("environment") == "dirty" and not args.force:
                    print(
                        "refusing to write a dirty record without --force",
                        file=sys.stderr,
                    )
                    raise SystemExit(1)
                args.json.parent.mkdir(parents=True, exist_ok=True)
                with args.json.open("a", encoding="utf-8") as fh:
                    fh.write(json.dumps(cell) + "\n")


if __name__ == "__main__":
    main()
