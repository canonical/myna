"""Aggregate bench records into a cross-label comparison table.

    myna-bench summarize --in results.jsonl --by-category

Reads the JSONL written by the sweep runner and produces the model x hardware
comparison the specs need: one row per label (e.g. ``myna-whisper/cpu/tiny/batch``),
with micro-averaged WER/CER (total edits / total reference, so long clips count
proportionally) and finalize-latency percentiles.

Records are deduplicated by (label, clip, cold), keeping the most recent - so
re-running a label replaces its old rows rather than double-counting.
"""

from __future__ import annotations

import json
from pathlib import Path

# A row's identity in a results file. The machine is half of it: a leaderboard
# holds the same <snap>/<engine>/<model>/<mode> measured on many machines, and
# that is the whole point of collecting them. Keyed on label alone, merging two
# submissions kept one and silently dropped the other.
RowKey = tuple[str, str]


def machine_of(record: dict) -> str:
    """Which machine produced a row. ``unknown`` for rows written before
    provenance was stamped, so they group together instead of vanishing."""
    provenance = record.get("provenance")
    if isinstance(provenance, dict) and provenance.get("machine"):
        return str(provenance["machine"])
    return record.get("machine") or "unknown"


def row_key(record: dict) -> RowKey:
    return machine_of(record), record["label"]


def _load_latest(path: Path) -> tuple[list[dict], dict[RowKey, tuple[str, str]]]:
    """Return (clip records, {(machine, label): (status, reason)}), last wins.

    Cold samples are keyed separately so a clip measured both cold and warm
    keeps both rows rather than the warm run clobbering the cold one.

    Status records (``{"machine", "label", "status", "reason"}``, no "clip")
    come from the sweep runner: "usability_fail" when a target ran out of its
    wall-clock budget mid-sweep, "broken" when it crashed outright, "ok" on a
    clean completion. A run that didn't finish leaves fewer clip records for
    that label - indistinguishable, from clip records alone, from "this
    category wasn't scheduled". The status record is the one durable signal
    that a label's partial data means it *failed*, not that it scored a clean
    0%; last-occurrence-wins so a later clean rerun clears an earlier failure.
    """
    if not path.exists():
        raise SystemExit(f"no results at {path}")
    latest: dict[tuple[str, str, str, bool], dict] = {}
    statuses: dict[RowKey, tuple[str, str]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        rec = json.loads(raw)
        if rec.get("type") == "machine":
            continue
        if "status" in rec and "clip" not in rec:
            statuses[row_key(rec)] = (rec["status"], rec.get("reason", ""))
            continue
        if rec.get("error"):
            # The backend errored instead of transcribing (missing runtime
            # library, unloadable weights). Its empty hypothesis would score as
            # a flawless 100% WER and drag the label's micro-average with it.
            continue
        machine, label = row_key(rec)
        latest[(machine, label, rec["clip"], bool(rec.get("cold", False)))] = rec
    return list(latest.values()), statuses


def one_machine_per_name(records: list[dict]) -> None:
    """Refuse a file where one machine name covers two different CPUs.

    Hostnames are not unique across a team, and rows are keyed by name - two
    laptops both called "framework" would merge into one row and lose a
    submission, which is exactly the bug this key was widened to fix. Catch it
    rather than average across two machines.
    """
    cpus: dict[str, set[str]] = {}
    for record in records:
        provenance = record.get("provenance")
        cpu = provenance.get("cpu") if isinstance(provenance, dict) else None
        if cpu:
            cpus.setdefault(machine_of(record), set()).add(str(cpu))
    clashing = {name: sorted(seen) for name, seen in cpus.items() if len(seen) > 1}
    if clashing:
        detail = "; ".join(f"{name}: {seen}" for name, seen in sorted(clashing.items()))
        raise SystemExit(
            f"one machine name covers more than one CPU ({detail}) - two hosts share a "
            "hostname, so their rows would merge and one submission would be lost. "
            "Rename one host, or split the file."
        )


def one_corpus(records: list[dict], wanted: str | None) -> tuple[list[dict], str]:
    """Records for a single corpus. Two corpora in one file is not a table to
    be qualified, it is a comparison that cannot be made."""
    ids = {r.get("corpus_id") for r in records}
    if not ids:
        raise SystemExit("no clip records to aggregate")
    if None in ids:
        raise SystemExit(
            "records with no corpus_id: they were measured before the corpus was "
            "stamped, and nothing says what audio produced them - drop them"
        )
    if wanted is None:
        if len(ids) > 1:
            raise SystemExit(
                "records span " + ", ".join(sorted(ids)) + " - a WER micro-averaged "
                "across two corpora compares nothing; re-run on one, or pass --corpus"
            )
        wanted = ids.pop()
    return [r for r in records if r["corpus_id"] == wanted], wanted


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    return s[min(len(s) - 1, int(q * len(s)))]


def _summarize(records: list[dict]) -> dict[RowKey, dict]:
    """Group records by (machine, label) and micro-average the metrics.

    Accuracy and warm latency come from the warm rows; cold-load latency is
    reported separately from the cold samples (``--cold`` bench runs).
    """
    groups: dict[RowKey, list[dict]] = {}
    for rec in records:
        groups.setdefault(row_key(rec), []).append(rec)

    summary = {}
    for key, recs in groups.items():
        machine, label = key
        warm = [r for r in recs if not r.get("cold", False)]
        cold = [r for r in recs if r.get("cold", False)]
        finals = [r["finalize_latency"] for r in warm if r.get("finalize_latency") is not None]
        # Pure model-load wait (session open -> ready), independent of decode.
        cold_readys = [r["time_to_ready"] for r in cold if r.get("time_to_ready") is not None]
        warm_readys = [r["time_to_ready"] for r in warm if r.get("time_to_ready") is not None]
        rtfs = [r["rtf"] for r in warm if r.get("rtf") is not None]
        wer_edits = sum(r["wer_edits"] for r in warm)
        ref_words = sum(r["ref_words"] for r in warm)
        cer_edits = sum(r["cer_edits"] for r in warm)
        ref_chars = sum(r["ref_chars"] for r in warm)
        summary[key] = {
            "clips": len(warm),
            "label": label,
            "machine": machine,
            # None, never 0.0: a label whose warm rows all errored - or which
            # only ever ran as a --cold sample - has no score, and a 0.00%
            # would both print as flawless and rank first.
            "wer": wer_edits / ref_words if ref_words else None,
            "cer": cer_edits / ref_chars if ref_chars else None,
            "rtf": _pct(rtfs, 0.5),
            "median_final": _pct(finals, 0.5),
            "p95_final": _pct(finals, 0.95),
            # cold-load = model residency wait only (time_to_ready), from --cold
            # samples; the warm reload should be ~0.
            "cold_ready": max(cold_readys) if cold_readys else None,
            "warm_ready": _pct(warm_readys, 0.5),
            "audio": sum(r["audio_seconds"] for r in warm),
            "peak_rss_mb": None,
            "peak_vram_mb": None,
        }
    return summary


def _load_resources(path: Path) -> dict[RowKey, dict]:
    """Read the sweep's peak RAM/VRAM sidecar, keyed like every other row.

    Last occurrence wins. Absent file -> empty (resource columns are hidden).
    """
    if not path.exists():
        return {}
    peaks: dict[RowKey, dict] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw:
            rec = json.loads(raw)
            peaks[row_key(rec)] = rec
    return peaks


def resources_path_for(out: Path) -> Path:
    """Sidecar path for a results file. One rule, so writer and reader agree."""
    return out.parent / (out.stem + "-resources.jsonl")


def _f(x, spec: str = "6.2f") -> str:
    return format(x, spec) if isinstance(x, (int, float)) else "    --"


def _scaled(rate) -> float | None:
    """An error rate as a percentage, keeping "not scored" distinct from zero."""
    return rate * 100 if isinstance(rate, (int, float)) else None


def _speed(rtf) -> str:
    """Format RTF as a human-readable speed multiplier.

    0.018 -> '55x', 0.046 -> '22x', 1.682 -> '0.6x'.  Values >= 10x are
    shown as integers; below that one decimal keeps enough resolution to
    distinguish e.g. 5.2x from 4.8x.
    """
    if not isinstance(rtf, (int, float)) or rtf <= 0:
        return "   --"
    x = 1.0 / rtf
    return f"{x:.0f}x" if x >= 10 else f"{x:.1f}x"


# Ranking field per --sort choice. All of these are "lower is better" metrics
# (error rate, RTF, latency), so ascending sort puts the best performer first
# uniformly - no special-casing a "higher is better" field.
RANK_FIELDS = {
    "wer": "wer",
    "cer": "cer",
    "speed": "rtf",
    "latency": "median_final",
    "cold-load": "cold_ready",
}


def ranked_labels(
    summary: dict[RowKey, dict], sort: str, statuses: dict[RowKey, tuple[str, str]]
) -> list[RowKey]:
    """Rows ordered best-first by ``sort``; missing values sort last.

    A row whose last recorded status is not "ok" (usability_fail or broken)
    sinks below every clean completion regardless of metric value - its
    WER/speed was measured on however many clips it got through before failing,
    not the full sweep, so it is not a comparable data point and must never
    rank as if it beat a target that actually finished.
    """

    def failed(key: RowKey) -> bool:
        return statuses.get(key, ("ok", ""))[0] != "ok"

    if sort == "label":
        return sorted(summary, key=lambda key: (failed(key), key[1], key[0]))
    field = RANK_FIELDS[sort]
    return sorted(
        summary,
        key=lambda key: (
            failed(key),
            summary[key].get(field) is None,
            summary[key].get(field) or 0.0,
            key,
        ),
    )


def _print_overall(
    summary: dict[RowKey, dict], order: list[RowKey], statuses: dict[RowKey, tuple[str, str]]
) -> None:
    # The machine column is unconditional once a file holds more than one: on a
    # leaderboard the machine is half of what a row means, and a table that
    # hides it invites reading two hosts' numbers as a single ranking.
    machines = {key[0] for key in summary}
    show_machine = len(machines) > 1 or any(m != "unknown" for m in machines)
    show_res = any(s.get("peak_rss_mb") for s in summary.values())
    # Sized to the data, not to a guess: labels grew from "cpu/small" to
    # "whisper/cpu/base/streaming" when the runner started reporting the engine,
    # model and mode it actually measured, and a fixed 20 sheared every column
    # to the right of it.
    lw = max([len("label"), *(len(key[1]) for key in summary)])
    mw = max([len("machine"), *(len(key[0]) for key in summary)]) if show_machine else 0
    mh = f"{'machine':{mw}} " if show_machine else ""
    rh = f"{'RSS MB':>9} {'VRAM MB':>9}" if show_res else ""
    print(
        f"{'#':>3} {'label':{lw}} {'status':>13} {mh}{'clips':>5} "
        f"{'WER%':>7} {'CER%':>7} {'speed':>6} "
        f"{'med final':>10} {'p95 final':>10} {'cold load':>10} {rh}"
    )
    print("-" * (lw + 86 + (mw + 1 if show_machine else 0) + (20 if show_res else 0)))
    for rank, key in enumerate(order, start=1):
        s = summary[key]
        status, reason = statuses.get(key, ("--", ""))
        status_col = status.upper() if status != "--" else "--"
        if reason:
            status_col = f"{status_col} ({reason[:20]})"
        mc = f"{key[0]:{mw}} " if show_machine else ""
        rc = (
            f"{_f(s.get('peak_rss_mb'), '9.1f')} {_f(s.get('peak_vram_mb'), '9.1f')}"
            if show_res
            else ""
        )
        print(
            f"{rank:>3} {key[1]:{lw}} {status_col:>13} {mc}{s['clips']:>5} "
            f"{_f(_scaled(s['wer']), '7.2f')} {_f(_scaled(s['cer']), '7.2f')} "
            f"{_speed(s['rtf']):>6} "
            f"{_f(s['median_final'], '10.3f')} {_f(s['p95_final'], '10.3f')} "
            f"{_f(s['cold_ready'], '10.3f')} {rc}"
        )
    print(
        "\nmed/p95 final are seconds (end-of-audio -> committed text); "
        "speed = audio/decode (higher is faster)."
    )
    print("cold load = model residency wait (session open -> ready), from --cold runs.")
    print(
        "status: OK = clean full sweep; USABILITY_FAIL = ran out of budget mid-sweep (metrics"
        " are partial, not comparable); BROKEN = crashed; -- = no status record for this row."
        " Failed/broken rows always sort last regardless of --sort."
    )
    if show_res:
        print("RSS/VRAM = peak memory during the run.")


def _print_by_category(
    records: list[dict], order: list[RowKey], statuses: dict[RowKey, tuple[str, str]]
) -> None:
    records = [r for r in records if not r.get("cold", False)]  # warm only
    present = {row_key(r) for r in records}
    keys = [key for key in order if key in present]
    cats = sorted({r["category"] for r in records})
    # micro WER per (row, category)
    cell: dict[tuple[RowKey, str], tuple[int, int]] = {}
    for r in records:
        index = (row_key(r), r["category"])
        e, w = cell.get(index, (0, 0))
        cell[index] = (e + r["wer_edits"], w + r["ref_words"])

    show_machine = len({key[0] for key in keys}) > 1
    names = [f"{key[1]} @ {key[0]}" if show_machine else key[1] for key in keys]
    # Rows on Y, categories on X. Column width from the widest category name.
    lw = max([len("label"), *(len(n) for n in names)])
    cw = max(6, *(len(c) for c in cats)) if cats else 6
    print("\nWER% by category ('--' = no clips scored in that category, not a 0% pass)")
    header = f"{'label':{lw}} " + " ".join(f"{cat:>{cw}}" for cat in cats)
    print(header)
    print("-" * len(header))
    for key, name in zip(keys, names, strict=True):
        status, _ = statuses.get(key, ("--", ""))
        cells = []
        for cat in cats:
            e, w = cell.get((key, cat), (0, 0))
            cells.append(f"{'--':>{cw}}" if not w else f"{e / w * 100:>{cw}.1f}")
        marker = f" [{status.upper()}]" if status not in ("ok", "--") else ""
        print(f"{name:{lw}} " + " ".join(cells) + marker)


def cmd_summarize(args) -> None:  # noqa: ANN001
    infile = Path(args.infile)
    records, statuses = _load_latest(infile)
    records, corpus = one_corpus(records, getattr(args, "corpus", None))
    one_machine_per_name(records)
    summary = _summarize(records)
    for key, peaks in _load_resources(resources_path_for(infile)).items():
        if key in summary:
            summary[key]["peak_rss_mb"] = peaks.get("peak_rss_mb")
            summary[key]["peak_vram_mb"] = peaks.get("peak_vram_mb")
    machines = {key[0] for key in summary}
    print(
        f"{len(records)} records across {len(summary)} row(s) "
        f"on {len(machines)} machine(s) from {infile}"
    )
    print(f"corpus {corpus}\n")
    order = ranked_labels(summary, getattr(args, "sort", "wer") or "wer", statuses)
    _print_overall(summary, order, statuses)
    if getattr(args, "by_category", False):
        _print_by_category(records, order, statuses)


# ---------------------------------------------------------------------------
# `merge`: fold submissions into the tracked leaderboard
# ---------------------------------------------------------------------------


def _rows_of(path: Path) -> list[dict]:
    """Every non-header record in a results file, in order."""
    if not path.exists():
        raise SystemExit(f"no results at {path}")
    rows = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw and json.loads(raw).get("type") != "machine":
            rows.append(json.loads(raw))
    return rows


def cmd_merge(args) -> None:  # noqa: ANN001
    """Append submissions to a leaderboard file, replacing each machine's rows.

    A leaderboard is one file tracked in git, so the merge is deliberately
    boring: read what is there, drop everything the incoming machines already
    contributed, append the new rows, write it back. Re-submitting from the
    same machine replaces that machine's rows rather than doubling them, and no
    other machine's rows are touched.

    Two guards, because both failures are silent otherwise. A submission
    measured against a different corpus cannot be compared with what is already
    there, and two hosts sharing a hostname would merge into one row and lose a
    submission.
    """
    out = Path(args.leaderboard)
    existing = _rows_of(out) if out.exists() else []
    incoming: list[dict] = []
    for name in args.results:
        rows = _rows_of(Path(name))
        if not rows:
            print(f"{name}: no records, skipping")
            continue
        incoming += rows

    if not incoming:
        raise SystemExit("nothing to merge")

    corpora = {r.get("corpus_id") for r in incoming + existing if "clip" in r}
    corpora.discard(None)
    if len(corpora) > 1 and not args.allow_mixed_corpora:
        raise SystemExit(
            "submissions span more than one corpus (" + ", ".join(sorted(corpora)) + ") - a "
            "leaderboard averaged across two corpora ranks nothing. Re-run against the corpus "
            "the leaderboard uses, or pass --allow-mixed-corpora and always summarize with "
            "--corpus <id>."
        )

    one_machine_per_name([r for r in incoming + existing if "clip" in r])

    submitting = {machine_of(r) for r in incoming}
    kept = [r for r in existing if machine_of(r) not in submitting]
    replaced = len(existing) - len(kept)

    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w", encoding="utf-8") as fp:
        for row in kept + incoming:
            fp.write(json.dumps(row) + "\n")

    print(f"merged {len(incoming)} record(s) from {', '.join(sorted(submitting))}")
    if replaced:
        print(f"replaced {replaced} earlier record(s) from the same machine(s)")
    print(f"{len(kept) + len(incoming)} record(s) now in {out}")
