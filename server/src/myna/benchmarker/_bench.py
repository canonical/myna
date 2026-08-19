"""Per-clip scoring: run one WAV through a snap socket and produce a record row.

The record schema is identical to dev/bench.py so dev/aggregate.py and the
``summarize`` subcommand work against benchmarker output unchanged.
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

from myna.core import SessionConfig, WsUnixClient
from myna.testbed import Harness, character_error_rate, word_error_rate
from myna.testbed.adapter import Candidate
from myna.testbed.corpus import Clip


def session_error(record) -> dict | None:
    """Return the ``transcription.error`` payload if the session failed."""
    for te in record.events:
        if te.event.type == "transcription.error":
            return {
                "code": getattr(te.event, "code", None),
                "message": getattr(te.event, "message", None),
            }
    return None


async def bench_clip(socket: Path, clip: Clip, label: str, *, streaming: bool, batch: bool):
    """Run one clip against the socket; return (record, wer, cer)."""
    source = clip.open_source(realtime=not batch)
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
) -> tuple[bool, int]:
    """Sweep ``clips`` and append JSONL records to ``out_fp``.

    Returns ``(overran, scored)`` — overran is True when the budget was
    exceeded before all clips completed.
    """
    import time

    served_models: list[str] = []
    try:
        caps = await WsUnixClient(socket).capabilities()
        served_models = list(caps.models)
    except Exception:  # noqa: BLE001
        pass

    run_started = datetime.now(UTC).isoformat()
    lines: list[dict] = []
    failed: list[dict] = []
    tot_edits = tot_words = 0
    tot_audio = 0.0
    finals: list[float] = []
    overran = False
    wall_start = time.monotonic()

    print(f"label={label}  clips={len(clips)}  socket={socket}")
    print(
        f"{'clip':24} {'category':10} {'WER%':>6} {'CER%':>6}"
        f" {'audio s':>8} {'ready s':>8} {'final s':>8}"
    )
    print("-" * 84)

    for index, clip in enumerate(clips):
        if budget_seconds and time.monotonic() - wall_start > budget_seconds:
            overran = True
            elapsed = time.monotonic() - wall_start
            print(
                f"budget exceeded after {index}/{len(clips)} clips "
                f"({elapsed:.0f}s > {budget_seconds:.0f}s) - stopping"
            )
            break

        record, wer, cer = await bench_clip(socket, clip, label, streaming=streaming, batch=True)
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
        )
        lines.append(line)

        if line["error"]:
            failed.append(line)
            print(f"{clip.id:24} {clip.category:10} {'FAILED':>6} {line['error']['code']}")
            continue

        tot_edits += wer.substitutions + wer.deletions + wer.insertions
        tot_words += wer.reference_length
        tot_audio += line["audio_seconds"]
        if line["finalize_latency"] is not None:
            finals.append(line["finalize_latency"])
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
    if tot_words:
        print(
            f"micro-averaged WER : {tot_edits / tot_words * 100:.2f}%"
            f"  ({tot_edits} edits / {tot_words} ref words)"
        )
    if finals:
        median_final = sorted(finals)[len(finals) // 2]
        print(f"median finalize    : {median_final:.3f}s  (end-of-audio -> committed text)")
    print(f"audio streamed     : {tot_audio:.1f}s total")
    if overran:
        print(
            f"USABILITY FAIL     : scored {scored}/{len(clips)} clips within {budget_seconds:.0f}s"
        )

    return overran, scored
