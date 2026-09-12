"""Per-clip scoring: run WAVs through a socket and produce the record rows.

One source of truth for WER/CER and latency. The sweep runner calls
``run_clips`` in-process; ``cmd_bench`` exposes the same code as the ``bench``
subcommand for driving a socket that is already up (no install, no purge).

Scoring itself lives in ``myna.testbed.metrics`` - this module only decides
what gets measured and what a record row contains.
"""

from __future__ import annotations

import asyncio
import json
import time
from datetime import UTC, datetime
from pathlib import Path

from myna.core import SessionConfig, WsUnixClient
from myna.testbed import Harness, character_error_rate, word_error_rate
from myna.testbed.adapter import Candidate
from myna.testbed.corpus import Clip


class AllClipsFailed(Exception):
    """Every clip errored: the target is misconfigured, not merely bad.

    Distinct from a poor score. An erroring backend emits no transcript, and an
    empty hypothesis scores a plausible 100% WER that is indistinguishable from
    a model that ran and was terrible - so the sweep must report the target
    broken rather than bank the rows.
    """


def session_error(record) -> dict | None:
    """The backend's ``transcription.error``, if the session failed."""
    for te in record.events:
        if te.event.type == "transcription.error":
            return {
                "code": getattr(te.event, "code", None),
                "message": getattr(te.event, "message", None),
            }
    return None


async def bench_clip(
    socket: Path, clip: Clip, label: str, *, streaming: bool, realtime: bool = False
):
    """Run one clip against the socket; return (record, wer, cer).

    ``realtime`` paces the feed like live dictation. The sweep feeds as fast as
    the socket accepts, which is what makes a full matrix affordable, but a
    long clip fed flat out can outrun a backend's websocket keepalive - so the
    pacing stays available rather than being compiled out.
    """
    source = clip.open_source(realtime=realtime)
    record = await Harness().run(
        client=WsUnixClient(socket),
        candidate=Candidate(
            model=label,
            engine="socket",
            streaming_strategy="streaming" if streaming else "batch",
        ),
        source=source,
        config=SessionConfig(audio_format=source.format, language=clip.language),
    )
    wer = word_error_rate(clip.text, record.transcript)
    cer = character_error_rate(clip.text, record.transcript)
    return record, wer, cer


def to_line(
    clip: Clip,
    record,
    wer,
    cer,
    *,
    label: str,
    cold: bool,
    run_started: str,
    served_models: list[str],
    usability_fail: bool,
    clips_scored: int,
    clips_requested: int,
    provenance: dict | None,
    corpus: dict[str, str] | None = None,
) -> dict:
    """Serialise a single-clip result to the JSONL record schema."""
    m = record.metrics
    error = session_error(record)
    line: dict = {
        "error": error,
        "label": label,
        "cold": cold,
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
        "run_started": run_started,
        "served_models": served_models,
        **(corpus or {}),
        # Stamped on every row of a truncated sweep: coverage travels with the
        # data, so a partial WER can never be read as a full one.
        "usability_fail": usability_fail,
        "clips_scored": clips_scored,
        "clips_requested": clips_requested,
    }
    if provenance is not None:
        line["provenance"] = provenance
    return line


def _fmt(x, spec: str = "6.2f") -> str:
    return format(x, spec) if isinstance(x, (int, float)) else "   -- "


async def run_clips(
    *,
    socket: Path,
    clips: list[Clip],
    label: str,
    cold: bool,
    streaming: bool,
    provenance: dict | None,
    budget_seconds: float | None,
    out_fp,
    corpus: dict[str, str] | None = None,
    realtime: bool = False,
) -> tuple[bool, int]:
    """Sweep ``clips`` and append JSONL records to ``out_fp``.

    Returns ``(overran, scored)`` - overran is True when the budget was
    exceeded before all clips completed. Raises ``AllClipsFailed`` when no clip
    produced a transcript.
    """
    served_models: list[str] = []
    try:
        # Ask the server what model it actually serves, so the *weight version*
        # travels with the data instead of only the human label: adapters report
        # a versioned id in capabilities (e.g. whisper-base@<commit>).
        caps = await WsUnixClient(socket).capabilities()
        served_models = list(caps.models)
    except Exception as exc:  # noqa: BLE001 - discovery is advisory
        print(f"(capabilities query failed: {type(exc).__name__}: {exc})")

    run_started = datetime.now(UTC).isoformat()
    lines: list[dict] = []
    failed: list[dict] = []
    tot_edits = tot_words = 0
    tot_audio = 0.0
    finals: list[float] = []
    readys: list[float] = []
    overran = False
    wall_start = time.monotonic()

    pace = "real-time pace" if realtime else "fast as possible"
    print(f"label={label}  clips={len(clips)}  socket={socket}")
    print(f"feeding audio at {pace}")
    # 'audio s' is how long the clip takes to stream (the bulk of the per-line
    # wait at real-time pace); 'ready s' is the cold model-load wait (session
    # open -> ready); 'final s' is end-of-audio -> committed text.
    print(
        f"{'clip':24} {'category':10} {'WER%':>6} {'CER%':>6}"
        f" {'audio s':>8} {'ready s':>8} {'final s':>8}"
    )
    print("-" * 84)

    for index, clip in enumerate(clips):
        if budget_seconds and time.monotonic() - wall_start > budget_seconds:
            # A backend slower than the budget is a usability verdict, not a
            # datapoint worth waiting out. Stop here rather than being killed
            # from outside, so the clips that did land still get written.
            overran = True
            elapsed = time.monotonic() - wall_start
            print(
                f"budget exceeded after {index}/{len(clips)} clips "
                f"({elapsed:.0f}s > {budget_seconds:.0f}s) - stopping"
            )
            break

        record, wer, cer = await bench_clip(
            socket, clip, label, streaming=streaming, realtime=realtime
        )
        line = to_line(
            clip,
            record,
            wer,
            cer,
            label=label,
            cold=cold,
            run_started=run_started,
            served_models=served_models,
            usability_fail=overran,
            clips_scored=0,  # back-patched below
            clips_requested=len(clips),
            provenance=provenance,
            corpus=corpus,
        )
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

    scored = len(lines) - len(failed)
    # Back-patch usability_fail and clips_scored now that we know the final values.
    for line in lines:
        line["usability_fail"] = overran
        line["clips_scored"] = scored

    for line in lines:
        out_fp.write(line)

    print("-" * 84)
    if failed:
        codes = ", ".join(sorted({ln["error"]["code"] for ln in failed}))
        print(f"FAILED             : {len(failed)}/{len(clips)} clips  ({codes})")
        print(f"  {failed[0]['error']['message']}")
    if tot_words:
        print(
            f"micro-averaged WER : {tot_edits / tot_words * 100:.2f}%"
            f"  ({tot_edits} edits / {tot_words} ref words)"
        )
    else:
        # Printing "0.00%" here would be the friendliest possible lie.
        print("micro-averaged WER : n/a  (no clip produced a transcript)")
    if readys:
        # The first clip carries the cold-load cost; report it distinctly.
        print(
            f"time to ready      : first={readys[0]:.3f}s"
            f"  median={sorted(readys)[len(readys) // 2]:.3f}s"
            + ("  (cold sample)" if cold else "")
        )
    if finals:
        median_final = sorted(finals)[len(finals) // 2]
        print(f"median finalize    : {median_final:.3f}s  (end-of-audio -> committed text)")
    print(f"audio streamed     : {tot_audio:.1f}s total")
    if overran:
        print(
            f"USABILITY FAIL     : scored {scored}/{len(clips)} clips within {budget_seconds:.0f}s"
        )

    if failed and not tot_words:
        raise AllClipsFailed(f"{label}: every clip failed ({failed[0]['error']['code']})")

    return overran, scored


# ---------------------------------------------------------------------------
# `bench` subcommand: score an already-running socket, install nothing
# ---------------------------------------------------------------------------


class _JsonlFile:
    """Minimal append-only writer, matching the runner's out_fp protocol."""

    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self._fp = path.open("a", encoding="utf-8")

    def write(self, record: dict) -> None:
        self._fp.write(json.dumps(record) + "\n")
        self._fp.flush()

    def close(self) -> None:
        self._fp.close()


def cmd_bench(args) -> None:  # noqa: ANN001
    """Sweep a manifest against one socket that is already serving."""
    from myna.testbed.corpus import load_manifest, verify_corpus

    manifest = Path(args.manifest)
    if not manifest.exists():
        raise SystemExit(f"manifest not found: {manifest}")
    clips = list(load_manifest(manifest))
    if args.clip:
        by_id = {c.id: c for c in clips}
        missing = [cid for cid in args.clip if cid not in by_id]
        if missing:
            raise SystemExit(
                f"unknown clip(s): {', '.join(missing)}; available: {', '.join(sorted(by_id))}"
            )
        clips = [by_id[cid] for cid in args.clip]
    if args.category:
        clips = [c for c in clips if c.category == args.category]
    if not clips:
        raise SystemExit("no clips selected")

    # Which corpus produced these numbers, recomputed from the clips rather
    # than taken on trust: a WER is only comparable against the same one.
    try:
        corpus = {
            "corpus_id": verify_corpus(manifest),
            "corpus_manifest": manifest.name,
        }
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc

    out = Path(args.out)
    fp = _JsonlFile(out)
    try:
        overran, scored = asyncio.run(
            run_clips(
                socket=Path(args.socket),
                clips=clips,
                label=args.label,
                cold=args.cold,
                streaming=args.streaming,
                provenance=json.loads(args.provenance) if args.provenance else None,
                budget_seconds=args.budget_seconds,
                out_fp=fp,
                corpus=corpus,
                realtime=args.realtime,
            )
        )
    except AllClipsFailed as exc:
        raise SystemExit(str(exc)) from exc
    finally:
        fp.close()
    print(f"wrote records to {out}")
    if overran:
        # Exit 2, distinct from 1: the backend worked, it was just too slow.
        raise SystemExit(2)
