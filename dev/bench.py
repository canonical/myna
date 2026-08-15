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
import time
from datetime import UTC, datetime
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
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
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
    clips = load_manifest(args.manifest)
    if args.clip:
        by_id = {c.id: c for c in clips}
        missing = [cid for cid in args.clip if cid not in by_id]
        if missing:
            raise SystemExit(
                f"unknown clip(s): {', '.join(missing)}; available: {', '.join(sorted(by_id))}"
            )
        clips = tuple(by_id[cid] for cid in args.clip)
    if args.category:
        clips = tuple(c for c in clips if c.category == args.category)
    if not clips:
        raise SystemExit("no clips selected")
    return clips


def session_error(record) -> dict | None:
    """The backend's ``transcription.error``, if the session failed.

    A backend that cannot run at all (missing runtime library, unloadable
    weights) still terminates the session cleanly - it just emits an error
    instead of a transcript. Scoring that as an empty hypothesis produces a
    perfectly plausible 100% WER row, indistinguishable from a model that ran
    and was terrible. Surface it instead.
    """
    for te in record.events:
        if te.event.type == "transcription.error":
            return {
                "code": getattr(te.event, "code", None),
                "message": getattr(te.event, "message", None),
            }
    return None


async def bench_clip(args, clip):
    """Run one clip against the socket; return (record, wer, cer)."""
    source = clip.open_source(realtime=not args.batch)
    streaming_strategy = "streaming" if args.streaming else "batch"
    record = await Harness().run(
        client=WsUnixClient(args.socket),
        candidate=Candidate(
            model=args.label, engine="socket", streaming_strategy=streaming_strategy
        ),
        source=source,
        config=SessionConfig(audio_format=source.format, language=clip.language),
    )
    wer = word_error_rate(clip.text, record.transcript)
    cer = character_error_rate(clip.text, record.transcript)
    return record, wer, cer


def to_line(args, clip, record, wer, cer) -> dict:
    m = record.metrics
    error = session_error(record)
    return {
        "error": error,
        "label": args.label,
        "cold": args.cold,
        "clip": clip.id,
        "category": clip.category,
        "language": clip.language,
        "reference": clip.text,
        "transcript": record.transcript,
        "wer": round(wer.rate, 4),
        "cer": round(cer.rate, 4),
        "edits": {
            "sub": wer.substitutions,
            "del": wer.deletions,
            "ins": wer.insertions,
        },
        # raw counts so the aggregator can micro-average across clips
        "wer_edits": wer.substitutions + wer.deletions + wer.insertions,
        "ref_words": wer.reference_length,
        "cer_edits": cer.substitutions + cer.deletions + cer.insertions,
        "ref_chars": cer.reference_length,
        "audio_seconds": round(record.audio_duration_seconds, 3),
        "time_to_first_event": m.time_to_first_event,
        "time_to_ready": m.time_to_ready,
        "time_to_first_snippet": m.time_to_first_snippet,
        "time_to_first_final": m.time_to_first_final,
        "time_to_first_committed": m.time_to_first_committed,
        "time_to_first_unstable": m.time_to_first_unstable,
        "time_to_terminal": m.time_to_terminal,
        "finalize_latency": m.finalize_latency,
        "rtf": round(m.rtf, 4) if m.rtf is not None else None,
        "commit_stability": m.commit_stability,
        "committed_segments": m.committed_segments,
        "streaming_strategy": record.candidate.streaming_strategy,
        "started_at": record.started_at,
    }


def _fmt(x, spec="6.2f"):
    return format(x, spec) if isinstance(x, (int, float)) else "   -- "


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("clip", nargs="*", help="clip ids (default: all in the manifest)")
    parser.add_argument("--socket", required=True, type=Path)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=REPO_ROOT / "server" / "fixtures" / "manifest.json",
        help="corpus manifest to sweep (default: synthetic fixtures; "
        "use corpus/real/manifest.json for trustworthy WER)",
    )
    parser.add_argument(
        "--label",
        help=(
            "tag for this run (default: active engine name, e.g. 'cpu');"
            " pass 'cpu/small' to record the model too"
        ),
    )
    parser.add_argument("--category", help="only clips in this UD129 category")
    parser.add_argument("--batch", action="store_true", help="stream as fast as possible")
    parser.add_argument(
        "--streaming",
        action="store_true",
        help="enable streaming mode (progressive committed segments)",
    )
    parser.add_argument(
        "--cold",
        action="store_true",
        help="tag records as a cold-load sample (first request after a restart)",
    )
    parser.add_argument(
        "--provenance",
        help=(
            "JSON object merged into every record"
            " (e.g. hardware/engine metadata from the matrix runner)"
        ),
    )
    parser.add_argument(
        "--budget-seconds",
        type=float,
        default=None,
        help=(
            "wall-clock budget for the sweep; overrunning it stops early and"
            " marks the run a usability failure (exit 2) rather than waiting out"
            " a backend that is unusably slower than real time"
        ),
    )
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "results" / "bench.jsonl")
    args = parser.parse_args()
    if args.label is None:
        args.label = detect_label()
    provenance = json.loads(args.provenance) if args.provenance else None

    # Ask the server what model it actually serves, so the *weight version*
    # travels with the data instead of only the human --label (todo.txt /
    # labelling): adapters report a versioned id in capabilities (e.g.
    # whisper-base@<commit>). Best-effort — an old server or one without a
    # capabilities surface just yields no served_models.
    served_models: list[str] = []
    try:
        caps = await WsUnixClient(args.socket).capabilities()
        served_models = list(caps.models)
    except Exception as exc:  # noqa: BLE001 — discovery is advisory
        print(f"(capabilities query failed: {type(exc).__name__}: {exc})")

    clips = select_clips(args)
    pace = (
        "fast as possible"
        if args.batch
        else "real-time pace (audio streams in full before finalize)"
    )
    print(f"label={args.label}  clips={len(clips)}  socket={args.socket}")
    print(f"feeding audio at {pace}")
    # 'audio s' is how long the clip takes to stream (the bulk of the per-line
    # wait at real-time pace); 'ready s' is the cold model-load wait (session
    # open -> ready); 'final s' is end-of-audio -> committed text.
    print(
        f"{'clip':24} {'category':10} {'WER%':>6} {'CER%':>6}"
        f" {'audio s':>8} {'ready s':>8} {'final s':>8}"
    )
    print("-" * 84)

    lines = []
    tot_edits = tot_words = 0
    tot_audio = 0.0
    finals: list[float] = []
    readys: list[float] = []
    failed: list[dict] = []
    overran = False
    started = time.monotonic()
    for index, clip in enumerate(clips):
        if args.budget_seconds and time.monotonic() - started > args.budget_seconds:
            # A backend slower than the budget is a usability verdict, not a
            # datapoint worth waiting out. Stop here rather than being killed
            # from outside, so the clips that did land still get written.
            overran = True
            print(
                f"budget exceeded after {index}/{len(clips)} clips "
                f"({time.monotonic() - started:.0f}s > {args.budget_seconds:.0f}s) - stopping"
            )
            break
        record, wer, cer = await bench_clip(args, clip)
        line = to_line(args, clip, record, wer, cer)
        lines.append(line)
        if line["error"]:
            # Not a 100%-WER data point: the backend never ran. Keep it out of
            # the score entirely so a broken target can't masquerade as a bad
            # model in the aggregate.
            failed.append(line)
            print(f"{clip.id:24} {clip.category:10} {'FAILED':>6} {line['error']['code']}")
            continue
        tot_edits += wer.substitutions + wer.deletions + wer.insertions
        tot_words += wer.reference_length
        tot_audio += line["audio_seconds"]
        if line["finalize_latency"] is not None:
            finals.append(line["finalize_latency"])
        if line["time_to_ready"] is not None:
            readys.append(line["time_to_ready"])
        print(
            f"{clip.id:24} {clip.category:10} "
            f"{_fmt(wer.rate * 100)} {_fmt(cer.rate * 100)} "
            f"{_fmt(line['audio_seconds'], '8.2f')} "
            f"{_fmt(line['time_to_ready'], '8.3f')} "
            f"{_fmt(line['finalize_latency'], '8.3f')}"
        )

    print("-" * 84)
    if failed:
        codes = ", ".join(sorted({ln["error"]["code"] for ln in failed}))
        print(f"FAILED             : {len(failed)}/{len(clips)} clips  ({codes})")
        print(f"  {failed[0]['error']['message']}")
    median_final = sorted(finals)[len(finals) // 2] if finals else None
    if tot_words:
        micro_wer = tot_edits / tot_words * 100
        print(f"micro-averaged WER : {micro_wer:.2f}%  ({tot_edits} edits / {tot_words} ref words)")
    else:
        # Printing "0.00%" here would be the friendliest possible lie.
        print("micro-averaged WER : n/a  (no clip produced a transcript)")
    if readys:
        # The first clip carries the cold-load cost; report it distinctly.
        print(
            f"time to ready      : first={readys[0]:.3f}s"
            f"  median={sorted(readys)[len(readys) // 2]:.3f}s"
            + ("  (--cold sample)" if args.cold else "")
        )
    if median_final is not None:
        print(f"median finalize    : {median_final:.3f}s  (end-of-audio -> committed text)")
    print(
        f"audio streamed     : {tot_audio:.1f}s total"
        + ("" if args.batch else "  (use --batch to skip real-time pacing)")
    )

    if overran:
        print(
            f"USABILITY FAIL     : scored {len(lines) - len(failed)}/{len(clips)} clips"
            f" within {args.budget_seconds:.0f}s"
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    run_started = datetime.now(UTC).isoformat()
    with args.out.open("a", encoding="utf-8") as fp:
        for line in lines:
            record = {
                "run_started": run_started,
                "served_models": served_models,
                # Stamped on every row of a truncated sweep: coverage travels
                # with the data, so a partial WER can never be read as a full one.
                "usability_fail": overran,
                "clips_scored": len(lines) - len(failed),
                "clips_requested": len(clips),
                **line,
            }
            if provenance:
                record["provenance"] = provenance
            fp.write(json.dumps(record) + "\n")
    print(f"wrote {len(lines)} records to {args.out}")

    if failed and not tot_words:
        # Every clip errored: the target is misconfigured, not bad. Fail the
        # run so the matrix runner reports it instead of banking the rows.
        raise SystemExit(f"{args.label}: every clip failed ({failed[0]['error']['code']})")
    if overran:
        # Exit 2, distinct from 1: the backend worked, it was just too slow.
        budget = f"{args.budget_seconds:.0f}s"
        print(f"{args.label}: sweep exceeded its {budget} budget", file=sys.stderr)
        raise SystemExit(2)


if __name__ == "__main__":
    asyncio.run(main())
