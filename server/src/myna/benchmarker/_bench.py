"""Per-clip scoring: run WAVs through a socket and produce the record rows.

One source of truth for WER/CER and latency. The sweep runner calls
``run_clips`` in-process; ``cmd_bench`` exposes the same code as the ``bench``
subcommand for driving a socket that is already up (no install, no purge).

Scoring itself lives in ``myna.testbed.metrics`` - this module only decides
what gets measured and what a record row contains.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import time
from collections.abc import Mapping
from datetime import UTC, datetime
from pathlib import Path
from typing import Protocol

from myna.benchmarker._events import EventsFile, event_line, events_path_for
from myna.benchmarker._pace import MAX, REALTIME, feed_lag, paced_label, starved
from myna.benchmarker._schedule import MEASURED, WARMUP, Schedule, plan_clips
from myna.benchmarker._summarize import SCHEMA_VERSION, Record
from myna.core import SessionConfig, WsUnixClient
from myna.testbed import (
    NORMALIZER_VERSION,
    ErrorRate,
    Harness,
    ResultRecord,
    character_error_rate,
    keyword_score,
    word_error_rate,
)
from myna.testbed.adapter import Candidate
from myna.testbed.corpus import Clip
from myna.testbed.metrics import SECONDARY_NORMALIZER_VERSION, whisper_normalizer


class RecordSink(Protocol):
    def write(self, record: Mapping[str, object]) -> None: ...


class AllClipsFailed(Exception):
    """Every clip errored: the target is misconfigured, not merely bad.

    Distinct from a poor score. An erroring backend emits no transcript, and an
    empty hypothesis scores a plausible 100% WER that is indistinguishable from
    a model that ran and was terrible - so the sweep must report the target
    broken rather than bank the rows.
    """


def session_error(record: ResultRecord) -> dict[str, str | None] | None:
    """The backend's ``transcription.error``, if the session failed."""
    for te in record.events:
        if te.event.type == "transcription.error":
            return {
                "code": getattr(te.event, "code", None),
                "message": getattr(te.event, "message", None),
            }
    return None


async def bench_clip(
    socket: Path,
    clip: Clip,
    label: str,
    *,
    streaming: bool,
    realtime: bool = False,
    keywords: tuple[str, ...] = (),
) -> tuple[ResultRecord, ErrorRate, ErrorRate]:
    """Run one clip against the socket; return (record, wer, cer).

    ``realtime`` feeds on the capture clock, like live dictation (see
    ``_pace``); otherwise the feed is as fast as the socket accepts, which can
    outrun a backend's websocket keepalive on a long clip.
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
        config=SessionConfig(audio_format=source.format, language=clip.language, keywords=keywords),
    )
    wer = word_error_rate(clip.text, record.transcript)
    cer = character_error_rate(clip.text, record.transcript)
    return record, wer, cer


def _whisper_scores(clip: Clip, transcript: str) -> Record:
    """The row's secondary score, under Whisper's normaliser for the clip's
    language: comparable with the Open ASR Leaderboard, never a replacement
    for the primary ``wer``/``cer``. Counts ride along so it micro-averages."""
    std = whisper_normalizer(clip.language)
    wer = word_error_rate(clip.text, transcript, normalizer=std)
    cer = character_error_rate(clip.text, transcript, normalizer=std)
    return {
        "secondary_normalizer_version": SECONDARY_NORMALIZER_VERSION,
        "wer_whisper_norm": round(wer.rate, 4),
        "cer_whisper_norm": round(cer.rate, 4),
        "wer_whisper_norm_edits": wer.substitutions + wer.deletions + wer.insertions,
        "ref_words_whisper_norm": wer.reference_length,
        "cer_whisper_norm_edits": cer.substitutions + cer.deletions + cer.insertions,
        "ref_chars_whisper_norm": cer.reference_length,
    }


def to_line(
    clip: Clip,
    record: ResultRecord,
    wer: ErrorRate,
    cer: ErrorRate,
    *,
    label: str,
    cold: bool,
    repeat: int = 0,
    phase: str = MEASURED,
    run_started: str,
    served_models: list[str],
    usability_fail: bool,
    clips_scored: int,
    clips_requested: int,
    provenance: dict[str, object] | None,
    corpus: dict[str, str] | None = None,
    served_runtime: dict[str, str] | None = None,
    pace: str = MAX,
    keywords: tuple[str, ...] = (),
    keywords_sent: bool = False,
) -> Record:
    """Serialise a single-clip result to the JSONL record schema."""
    m = record.metrics
    error = session_error(record)
    lag = feed_lag(record.feed) if pace == REALTIME else None
    kw = keyword_score(clip.text, record.transcript, keywords)
    line: Record = {
        "schema_version": SCHEMA_VERSION,
        "error": error,
        "label": label,
        "cold": cold,
        # Which pass over the corpus, and whether it counts: warmup rows are
        # kept in the file but out of every aggregate.
        "repeat": repeat,
        "phase": phase,
        "clip": clip.id,
        "category": clip.category,
        "language": clip.language,
        "reference": clip.text,
        "transcript": record.transcript,
        "wer": round(wer.rate, 4),
        "cer": round(cer.rate, 4),
        # Scoring provenance: rows stamped under different normalize()
        # behavior must never be micro-averaged together.
        "normalizer_version": NORMALIZER_VERSION,
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
        **_whisper_scores(clip, record.transcript),
        # Scored whether or not they were sent, so a run without keywords is
        # the baseline for the same clips with them.
        "keywords": list(keywords),
        "keywords_sent": keywords_sent,
        "kw_in_reference": kw.in_reference,
        "kw_recalled": kw.recalled,
        "kw_recalled_cased": kw.recalled_cased,
        "kw_absent": kw.absent,
        "kw_false_hits": kw.false_hits,
        "kw_missed": list(kw.missed),
        "kw_false": list(kw.false),
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
        # How the audio was fed. A realtime row whose sender fell more than a
        # chunk behind the capture clock did not measure live latency.
        "pace": pace,
        "pace_lag": None if lag is None else round(lag, 4),
        "pace_starved": pace == REALTIME and starved(record.feed),
        "started_at": record.started_at,
        "run_started": run_started,
        "served_models": served_models,
        "served_runtime": served_runtime,
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


def _fmt(x: object, spec: str = "6.2f") -> str:
    return format(x, spec) if isinstance(x, (int, float)) else "   -- "


async def run_clips(
    *,
    socket: Path,
    clips: list[Clip],
    label: str,
    cold: bool,
    streaming: bool,
    provenance: dict[str, object] | None,
    budget_seconds: float | None,
    out_fp: RecordSink,
    corpus: dict[str, str] | None = None,
    realtime: bool = False,
    events_fp: RecordSink | None = None,
    schedule: Schedule | None = None,
    keywords: tuple[str, ...] | None = None,
    send_keywords: bool = False,
) -> tuple[bool, int]:
    """Sweep ``clips`` and append JSONL records to ``out_fp``.

    ``schedule`` runs its warmup clips first, then its repeats as shuffled
    passes (see ``_schedule``); a cold sample ignores it. The budget covers
    the measured passes only, and the clock starts at the first of them.

    A ``realtime`` run's label gains ``@realtime`` unless it has it already.

    ``events_fp``, when given, receives each clip's timed event stream as it
    lands, keyed by (label, clip, repeat, phase) like its row.
    Each clip is scored against ``keywords`` when given, else its own list;
    ``send_keywords`` also puts that list in the session.

    Returns ``(overran, scored)`` - overran is True when the budget was
    exceeded before all measured clips completed; scored counts measured rows
    only. Raises ``AllClipsFailed`` when no measured clip produced a transcript.
    """
    label = paced_label(label, REALTIME if realtime else MAX)
    plan = plan_clips(clips, schedule or Schedule(), cold=cold)
    measured = sum(1 for item in plan if item.phase != WARMUP)
    served_models: list[str] = []
    served_runtime: dict[str, str] | None = None
    try:
        # Ask the server what model it actually serves, so the *weight version*
        # travels with the data instead of only the human label: adapters report
        # a versioned id in capabilities (e.g. whisper-base@<commit>).
        caps = await WsUnixClient(socket).capabilities()
        served_models = list(caps.models)
        served_runtime = caps.runtime
    except Exception as exc:  # noqa: BLE001 - discovery is advisory
        print(f"(capabilities query failed: {type(exc).__name__}: {exc})")

    run_started = datetime.now(UTC).isoformat()
    machine = provenance.get("machine") if provenance else None
    machine = machine if isinstance(machine, str) else None
    lines: list[Record] = []
    failed: list[Record] = []
    tot_edits = tot_words = 0
    kw_ref = kw_hit = kw_hit_cased = kw_absent = kw_false = 0
    tot_audio = 0.0
    finals: list[float] = []
    readys: list[float] = []
    overran = False
    wall_start: float | None = None
    done = 0

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

    for item in plan:
        clip, warmup = item.clip, item.phase == WARMUP
        if not warmup and wall_start is None:
            wall_start = time.monotonic()
        elapsed = 0.0 if wall_start is None else time.monotonic() - wall_start
        if not warmup and budget_seconds and elapsed > budget_seconds:
            # A backend slower than the budget is a usability verdict, not a
            # datapoint worth waiting out. Stop here rather than being killed
            # from outside, so the clips that did land still get written.
            overran = True
            print(
                f"budget exceeded after {done}/{measured} clips "
                f"({elapsed:.0f}s > {budget_seconds:.0f}s) - stopping"
            )
            break

        scored_keywords = clip.keywords if keywords is None else keywords
        record, wer, cer = await bench_clip(
            socket,
            clip,
            label,
            streaming=streaming,
            realtime=realtime,
            keywords=scored_keywords if send_keywords else (),
        )
        line = to_line(
            clip,
            record,
            wer,
            cer,
            label=label,
            cold=cold,
            repeat=item.repeat,
            phase=item.phase,
            run_started=run_started,
            served_models=served_models,
            usability_fail=overran,
            clips_scored=0,  # back-patched below
            clips_requested=measured,
            provenance=provenance,
            corpus=corpus,
            served_runtime=served_runtime,
            pace=REALTIME if realtime else MAX,
            keywords=scored_keywords,
            keywords_sent=send_keywords,
        )
        lines.append(line)
        if events_fp is not None:
            events_fp.write(
                event_line(
                    record,
                    label=label,
                    clip=clip.id,
                    repeat=item.repeat,
                    phase=item.phase,
                    cold=cold,
                    machine=machine,
                )
            )

        done += 0 if warmup else 1
        tag = "  (warmup)" if warmup else f"  #{item.repeat}" if item.repeat else ""
        if line["error"]:
            # Not a 100%-WER data point: the backend never ran. Keep it out of
            # the score entirely so a broken target can't masquerade as a bad
            # model in the aggregate.
            if not warmup:
                failed.append(line)
            print(f"{clip.id:24} {clip.category:10} {'FAILED':>6} {line['error']['code']}{tag}")
            continue
        if warmup:
            print(f"{clip.id:24} {clip.category:10} {'--':>6} {'--':>6}{tag}")
            continue

        tot_edits += wer.substitutions + wer.deletions + wer.insertions
        tot_words += wer.reference_length
        tot_audio += line["audio_seconds"]
        kw_ref += line["kw_in_reference"]
        kw_hit += line["kw_recalled"]
        kw_hit_cased += line["kw_recalled_cased"]
        kw_absent += line["kw_absent"]
        kw_false += line["kw_false_hits"]
        if line["finalize_latency"] is not None:
            finals.append(line["finalize_latency"])
        if line["time_to_ready"] is not None:
            readys.append(line["time_to_ready"])
        print(
            f"{clip.id:24} {clip.category:10} "
            f"{_fmt(wer.rate * 100)} {_fmt(cer.rate * 100)} "
            f"{_fmt(line['audio_seconds'], '8.2f')} "
            f"{_fmt(line['time_to_ready'], '8.3f')} "
            f"{_fmt(line['finalize_latency'], '8.3f')}{tag}"
        )

    # The library versions are only reported once the server has loaded them,
    # so the runtime is read again now that the clips have run a session.
    try:
        served_runtime = (await WsUnixClient(socket).capabilities()).runtime
    except Exception as exc:  # noqa: BLE001 - discovery is advisory
        print(f"(runtime re-query failed: {type(exc).__name__}: {exc})")
    scored = sum(1 for line in lines if line["phase"] != WARMUP) - len(failed)
    # Back-patch usability_fail and clips_scored now that we know the final values.
    for line in lines:
        line["usability_fail"] = overran
        line["clips_scored"] = scored
        line["served_runtime"] = served_runtime

    for line in lines:
        out_fp.write(line)

    print("-" * 84)
    if failed:
        codes = ", ".join(sorted({ln["error"]["code"] for ln in failed}))
        print(f"FAILED             : {len(failed)}/{measured} clips  ({codes})")
        print(f"  {failed[0]['error']['message']}")
    if tot_words:
        print(
            f"micro-averaged WER : {tot_edits / tot_words * 100:.2f}%"
            f"  ({tot_edits} edits / {tot_words} ref words)"
        )
    else:
        # Printing "0.00%" here would be the friendliest possible lie.
        print("micro-averaged WER : n/a  (no clip produced a transcript)")
    if kw_ref:
        print(
            f"keyword recall     : {kw_hit / kw_ref * 100:.2f}%"
            f"  ({kw_hit}/{kw_ref}, as written {kw_hit_cased}/{kw_ref})"
        )
    if kw_absent:
        print(f"keyword false hits : {kw_false / kw_absent * 100:.2f}%  ({kw_false}/{kw_absent})")
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
    hungry = [ln for ln in lines if ln["pace_starved"]]
    if hungry:
        worst = max(ln["pace_lag"] for ln in hungry)
        print(
            f"STARVED FEED       : {len(hungry)} clip(s) fell over a chunk behind real time"
            f" (worst {worst:.3f}s); their latency is not live latency"
        )
    if overran:
        print(f"USABILITY FAIL     : scored {scored}/{measured} clips within {budget_seconds:.0f}s")

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

    def write(self, record: Mapping[str, object]) -> None:
        self._fp.write(json.dumps(record) + "\n")
        self._fp.flush()

    def close(self) -> None:
        self._fp.close()


def read_keyword_list(path: Path) -> tuple[str, ...]:
    """One keyword or phrase per line; blank lines and ``#`` comments skipped."""
    lines = (line.strip() for line in path.read_text(encoding="utf-8").splitlines())
    return tuple(line for line in lines if line and not line.startswith("#"))


def cmd_bench(args: argparse.Namespace) -> None:
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
    events = EventsFile(events_path_for(out))
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
                events_fp=events,
                keywords=read_keyword_list(Path(args.keyword_list)) if args.keyword_list else None,
                send_keywords=args.send_keywords,
            )
        )
    except AllClipsFailed as exc:
        raise SystemExit(str(exc)) from exc
    finally:
        fp.close()
        events.close()
    print(f"wrote records to {out} and event streams to {events_path_for(out)}")
    if overran:
        # Exit 2, distinct from 1: the backend worked, it was just too slow.
        raise SystemExit(2)
