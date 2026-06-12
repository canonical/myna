"""Sweep fixture clips against a running UbuSTT socket and score them.

    # whichever engine the snap has active is what you measure — label it:
    uv run python dev/bench.py --socket /var/snap/whisper/common/run/ubustt.sock \
        --label nvidia-gpu/small

    uv run python dev/bench.py --socket /tmp/ubustt.sock --category quiet quiet-weather

Runs each selected clip through the harness at real-time pace (like live
dictation), computes word/character error rate against the clip's reference
transcript (T06), prints a per-clip + summary table, and appends one JSON
record per clip to a results file (default: results/bench.jsonl).

The socket does not reveal which engine/model served the request, so the run
is tagged with whatever you pass to --label. Switch the snap's engine
(`sudo whisper use-engine cpu|nvidia-gpu`) and re-run with a new label to
compare. This is the seed of the T11 matrix runner.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def detect_label() -> str:
    """Best-effort: tag the run with the snap's active engine name.

    The socket itself can't tell us the engine, but the snap CLI can. The
    active *model* is not exposed by modelctl v2.0.0-beta.1 (and `status`
    trips on the ws+unix protocol gap), so pass --label cpu/small explicitly
    when the model matters.
    """
    try:
        out = subprocess.run(
            ["whisper", "show-engine", "--format=json"],
            capture_output=True, text=True, timeout=10, check=True,
        ).stdout
        return json.loads(out).get("name") or "socket"
    except Exception:
        return "socket"

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from myna.core import SessionConfig, WsUnixClient  # noqa: E402
from myna.testbed import (  # noqa: E402
    Harness,
    character_error_rate,
    load_manifest,
    word_error_rate,
)
from myna.testbed.adapter import Candidate  # noqa: E402


def select_clips(args: argparse.Namespace):
    clips = load_manifest(REPO_ROOT / "fixtures" / "manifest.json")
    if args.clip:
        by_id = {c.id: c for c in clips}
        missing = [cid for cid in args.clip if cid not in by_id]
        if missing:
            raise SystemExit(
                f"unknown clip(s): {', '.join(missing)}; "
                f"available: {', '.join(sorted(by_id))}"
            )
        clips = tuple(by_id[cid] for cid in args.clip)
    if args.category:
        clips = tuple(c for c in clips if c.category == args.category)
    if not clips:
        raise SystemExit("no clips selected")
    return clips


async def bench_clip(args, clip):
    """Run one clip against the socket; return (record, wer, cer)."""
    source = clip.open_source(realtime=not args.batch)
    record = await Harness().run(
        client=WsUnixClient(args.socket),
        candidate=Candidate(model=args.label, engine="socket", streaming_strategy="?"),
        source=source,
        config=SessionConfig(audio_format=source.format, language=clip.language),
    )
    wer = word_error_rate(clip.text, record.transcript)
    cer = character_error_rate(clip.text, record.transcript)
    return record, wer, cer


def to_line(args, clip, record, wer, cer) -> dict:
    m = record.metrics
    return {
        "label": args.label,
        "clip": clip.id,
        "category": clip.category,
        "language": clip.language,
        "reference": clip.text,
        "transcript": record.transcript,
        "wer": round(wer.rate, 4),
        "cer": round(cer.rate, 4),
        "edits": {"sub": wer.substitutions, "del": wer.deletions, "ins": wer.insertions},
        "ref_words": wer.reference_length,
        "audio_seconds": round(record.audio_duration_seconds, 3),
        "time_to_first_event": m.time_to_first_event,
        "finalize_latency": m.finalize_latency,
        "started_at": record.started_at,
    }


def _fmt(x, spec="6.2f"):
    return format(x, spec) if isinstance(x, (int, float)) else "   -- "


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("clip", nargs="*", help="clip ids (default: all fixtures)")
    parser.add_argument("--socket", required=True, type=Path)
    parser.add_argument("--label", help="tag for this run (default: active engine name, e.g. 'cpu'); pass 'cpu/small' to record the model too")
    parser.add_argument("--category", help="only clips in this UD129 category")
    parser.add_argument("--batch", action="store_true", help="stream as fast as possible")
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "results" / "bench.jsonl")
    args = parser.parse_args()
    if args.label is None:
        args.label = detect_label()

    clips = select_clips(args)
    print(f"label={args.label}  clips={len(clips)}  socket={args.socket}")
    print(f"{'clip':24} {'category':10} {'WER%':>6} {'CER%':>6} {'final s':>8} {'first s':>8}")
    print("-" * 66)

    lines = []
    tot_edits = tot_words = 0
    finals: list[float] = []
    for clip in clips:
        record, wer, cer = await bench_clip(args, clip)
        line = to_line(args, clip, record, wer, cer)
        lines.append(line)
        tot_edits += wer.substitutions + wer.deletions + wer.insertions
        tot_words += wer.reference_length
        if line["finalize_latency"] is not None:
            finals.append(line["finalize_latency"])
        print(
            f"{clip.id:24} {clip.category:10} "
            f"{_fmt(wer.rate * 100)} {_fmt(cer.rate * 100)} "
            f"{_fmt(line['finalize_latency'], '8.3f')} "
            f"{_fmt(line['time_to_first_event'], '8.3f')}"
        )

    print("-" * 66)
    micro_wer = (tot_edits / tot_words * 100) if tot_words else 0.0
    median_final = sorted(finals)[len(finals) // 2] if finals else None
    print(f"micro-averaged WER : {micro_wer:.2f}%  ({tot_edits} edits / {tot_words} ref words)")
    print(f"median finalize    : {_fmt(median_final, '.3f')}s" if median_final is not None else "")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    run_started = datetime.now(timezone.utc).isoformat()
    with args.out.open("a", encoding="utf-8") as fp:
        for line in lines:
            fp.write(json.dumps({"run_started": run_started, **line}) + "\n")
    print(f"wrote {len(lines)} records to {args.out}")


if __name__ == "__main__":
    asyncio.run(main())
