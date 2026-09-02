#!/usr/bin/env python3
"""Whisper streaming duty-cycle telemetry (whisper perf WP02/WP05).

    cd server && uv run --extra whisper python ../dev/whisper/bench_whisper_streaming.py \
        ../corpus/real/audio/librispeech-3081-166546-longform.wav \
        --model tiny --window 60 --json ../results/whisper-streaming.jsonl

    # sweep the re-decode cadence, which is WP05's only real lever:
    ... --cadence 1.0 2.0 3.0 4.0

    # replay a saved run without re-measuring:
    ... --replay ../results/whisper-streaming.jsonl

Drives a real ``FasterWhisperAdapter`` streaming session - the shipped path,
``run_session`` -> ``_run_streaming_session`` -> ``run_streaming_loop`` - over
a clip fed at **real-time pace** through ``LoopbackClient``, so the adapter
runs in-process and one ``StreamingTelemetry`` can be threaded into both the
adapter's constructor and ``Harness.run``. That is the only way to get these
numbers: the duty cycle is invisible on the wire (see ``StreamingTelemetry``).

Real-time pacing is not optional. ``duty_cycle`` divides encoder-busy time by
wall-clock session time, so a batch-fed session has no idle time to divide by
and reports ~100% regardless of what the policy does.

Whisper's encoder cost is a **per-call constant** (30 s of padded mel however
short the window - see WP03), so unlike Parakeet the interesting quantity here
is the decode *count*, not the audio-seconds redundancy. Both are printed;
``decode_calls`` is the one a cadence change moves.

The reported trade is deliberately two-sided. Cadence buys duty cycle and
costs responsiveness, so ``time_to_first_unstable`` is printed next to the
duty cycle rather than in a footnote: a policy that halves the duty cycle and
doubles the time before the user sees anything has not obviously won.

Before measuring, this calls ``bench_guard.check()`` with the ``whisper``
profile, the same way ``dev/whisper/bench_whisper.py`` does.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import sys
import time
import wave
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))
sys.path.insert(0, str(REPO_ROOT / "dev"))  # bench_guard.py lives there

import bench_guard  # noqa: E402
from myna.core import AudioFormat, LoopbackClient, PcmChunk, SessionConfig  # noqa: E402
from myna.testbed.harness import Harness, StreamingTelemetry  # noqa: E402
from myna.testbed.metrics import normalize, word_error_rate  # noqa: E402
from myna.testbed.whisper import WHISPER_RATE, FasterWhisperAdapter  # noqa: E402

PROFILE = bench_guard.PROFILES["whisper"]
SHIPPED_COMPUTE_TYPE = {"tiny": "int8"}  # everything else float32; see models/*/model.yaml
SHIPPED_CADENCE_S = 1.0  # FasterWhisperAdapter.stream_cadence_s


class _RealtimeClip:
    """A WAV file, optionally trimmed, streamed at real-time pace.

    ``myna.testbed.sources.WavFileSource`` has no trim, and the pacing here
    must always be real-time (see the module docstring), so this is its own
    small ``AudioSource`` - the same choice ``dev/parakeet/bench_streaming.py``
    made, for the same reason.
    """

    def __init__(self, path: Path, window: float = 0.0, chunk_seconds: float = 0.1) -> None:
        self._path = Path(path)
        self._window = window
        self._chunk_seconds = chunk_seconds
        with wave.open(str(self._path), "rb") as wav:
            if wav.getcomptype() != "NONE":
                raise ValueError(f"{self._path}: only uncompressed PCM WAV is supported")
            if wav.getnchannels() != 1 or wav.getframerate() != WHISPER_RATE:
                raise ValueError(
                    f"{self._path}: need {WHISPER_RATE} Hz mono, got "
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


async def run_one(
    wav: Path, window: float, model: str, compute_type: str, cadence: float, reference: str | None
) -> dict:
    telemetry = StreamingTelemetry()
    adapter = FasterWhisperAdapter(
        model,
        compute_type=compute_type,
        streaming=True,
        stream_cadence_s=cadence,
        stream_telemetry=telemetry,
    )
    # Warm the model before sampling faults: a cold load legitimately faults in
    # the whole file once, and that is not what the page-fault check is about.
    await adapter._load_model()  # noqa: SLF001 -- dev tooling, same precedent as bench_whisper.py

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
    wall = time.perf_counter() - t0
    faults = bench_guard.check_page_faults(majflt_before, bench_guard.sample_majflt())

    out = {
        "started_at": datetime.now(UTC).isoformat(),
        "wav": str(wav),
        "window_seconds": window or None,
        "model": model,
        "compute_type": compute_type,
        "cadence_seconds": cadence,
        "wall_seconds": wall,
        "transcript": record.transcript,
        "metrics": {
            "time_to_first_unstable": record.metrics.time_to_first_unstable,
            "time_to_first_committed": record.metrics.time_to_first_committed,
            "finalize_latency": record.metrics.finalize_latency,
        },
        "streaming_telemetry": record.to_json()["streaming_telemetry"],
        "page_fault_violation": str(faults) if faults else None,
    }
    if reference is not None:
        rate = word_error_rate(reference, record.transcript)
        out["wer"] = (
            (rate.substitutions + rate.deletions + rate.insertions)
            / max(len(normalize(reference).split()), 1)
            * 100
        )
    return out


def print_row(row: dict) -> None:
    summary = row["streaming_telemetry"]["summary"]
    metrics = row["metrics"]
    duty = summary["duty_cycle"]
    redundancy = summary["redundancy"]

    def fmt(value, spec, dash="   n/a"):
        return dash if value is None else format(value, spec)

    print(
        f"{row['cadence_seconds']:>8.1f}"
        f"{summary['decode_calls'].get('rolling', sum(summary['decode_calls'].values())):>9}"
        f"{fmt(duty and 100 * duty, '>8.1f'):>8}"
        f"{fmt(redundancy, '>9.2f')}"
        f"{summary['encoder_busy_seconds']:>10.2f}"
        f"{fmt(metrics['time_to_first_unstable'], '>10.2f')}"
        f"{fmt(metrics['finalize_latency'], '>10.2f')}"
        f"{fmt(row.get('wer'), '>8.2f')}"
    )


def print_header(row: dict) -> None:
    summary = row["streaming_telemetry"]["summary"]
    print(
        f"{row['model']}/{row['compute_type']}  "
        f"audio {summary['audio_seconds_ingested']:.1f}s  "
        f"session {summary['session_seconds']:.1f}s"
    )
    cols = ("cadence", "decodes", "duty%", "redund", "busy_s", "1st_show", "finalize", "WER%")
    print("".join(f"{c:>{w}}" for c, w in zip(cols, (8, 9, 8, 9, 10, 10, 10, 8), strict=True)))


def _reference_for(wav: Path, corpus: Path, manifest: str) -> str | None:
    """The manifest transcript for this clip, when it is a corpus clip and the
    whole clip is being used. A trimmed window has no matching reference, and
    scoring a 60 s window against a 300 s transcript would report ~80% WER for
    a perfect decode."""
    path = corpus / manifest
    if not path.exists():
        return None
    for clip in json.loads(path.read_text(encoding="utf-8"))["clips"]:
        if (corpus / clip["path"]).resolve() == wav.resolve():
            return clip["text"]
    return None


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("wav", nargs="?", type=Path, help="16 kHz mono PCM WAV clip")
    ap.add_argument("--model", default="tiny")
    ap.add_argument("--compute-type", default=None)
    ap.add_argument(
        "--window", type=float, default=0.0, help="trim to the first N seconds (0 = whole clip)"
    )
    ap.add_argument(
        "--cadence",
        type=float,
        nargs="+",
        default=[SHIPPED_CADENCE_S],
        help=f"re-decode cadences to sweep (shipped default {SHIPPED_CADENCE_S})",
    )
    ap.add_argument("--repeat", type=int, default=1, help="runs per cadence")
    ap.add_argument("--corpus", type=Path, default=REPO_ROOT / "corpus" / "real")
    ap.add_argument("--manifest", default="manifest-balanced.json")
    ap.add_argument("--json", type=Path, default=None, help="append each run as one JSON line")
    ap.add_argument("--force", action="store_true", help="measure despite guard violations")
    ap.add_argument("--replay", type=Path, default=None, help="print a saved file's rows")
    args = ap.parse_args()

    if args.replay is not None:
        rows = [
            json.loads(line)
            for line in args.replay.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        if not rows:
            raise SystemExit(f"{args.replay}: no records")
        print_header(rows[0])
        for row in rows:
            print_row(row)
        return

    if args.wav is None:
        ap.error("wav is required unless --replay is given")

    compute_type = args.compute_type or SHIPPED_COMPUTE_TYPE.get(args.model, "float32")
    violations = bench_guard.check(PROFILE)
    for v in violations:
        print(v, file=sys.stderr)
    if [v for v in violations if v.severity == bench_guard.HARD] and not args.force:
        raise SystemExit("refusing to measure on a contaminated machine; --force to override")

    # A trimmed window has no matching reference; see _reference_for.
    reference = None if args.window else _reference_for(args.wav, args.corpus, args.manifest)

    rows: list[dict] = []
    for cadence in args.cadence:
        for _ in range(args.repeat):
            row = asyncio.run(
                run_one(args.wav, args.window, args.model, compute_type, cadence, reference)
            )
            if row["page_fault_violation"]:
                print(row["page_fault_violation"], file=sys.stderr)
            if not rows:
                print_header(row)
            rows.append(row)
            print_row(row)

    if args.repeat > 1:
        print("\nmedian per cadence:")
        print_header(rows[0])
        for cadence in args.cadence:
            same = [r for r in rows if r["cadence_seconds"] == cadence]
            merged = dict(same[0])
            merged["streaming_telemetry"] = dict(same[0]["streaming_telemetry"])
            merged["streaming_telemetry"]["summary"] = dict(
                same[0]["streaming_telemetry"]["summary"]
            )
            for key in ("duty_cycle", "redundancy", "encoder_busy_seconds"):
                values = [
                    r["streaming_telemetry"]["summary"][key]
                    for r in same
                    if r["streaming_telemetry"]["summary"][key] is not None
                ]
                if values:
                    merged["streaming_telemetry"]["summary"][key] = statistics.median(values)
            merged["metrics"] = dict(same[0]["metrics"])
            for key in merged["metrics"]:
                values = [r["metrics"][key] for r in same if r["metrics"][key] is not None]
                if values:
                    merged["metrics"][key] = statistics.median(values)
            # `wer` is a top-level key, not one of `metrics` - taking the
            # median of everything under `metrics` and silently leaving this
            # one as run 1's value printed a first-run WER in a row labelled
            # median, which is exactly the kind of quiet mislabelling that
            # gets read as a result.
            wers = [r["wer"] for r in same if r.get("wer") is not None]
            if wers:
                merged["wer"] = statistics.median(wers)
            print_row(merged)

    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        with args.json.open("a", encoding="utf-8") as fh:
            for row in rows:
                fh.write(json.dumps(row) + "\n")


if __name__ == "__main__":
    main()
