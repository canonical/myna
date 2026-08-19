"""Aggregate bench records into a cross-label comparison table (T11).

    uv run python dev/aggregate.py
    uv run python dev/aggregate.py --by-category --in results/bench.jsonl

Reads the JSONL written by dev/bench.py and produces the model x hardware
comparison the specs need: one row per --label (e.g. cpu/tiny, cpu/small,
nvidia-gpu/small), with micro-averaged WER/CER (total edits / total
reference, so long clips count proportionally) and finalize-latency
percentiles.

Records are deduplicated by (label, clip), keeping the most recent — so
re-running a label replaces its old rows rather than double-counting.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


def load_latest(path: Path) -> tuple[list[dict], dict[str, tuple[str, str]]]:
    """Return (clip records, {label: (status, reason)}), last occurrence wins.

    Cold samples are keyed separately so a clip measured both cold and warm
    keeps both rows rather than the warm run clobbering the cold one.

    Status records (``{"label", "status", "reason"}``, no "clip") come from
    dev/matrix.py: "usability_fail" when a target ran out of its wall-clock
    budget mid-sweep, "broken" when it crashed outright, "ok" on a clean
    completion. A run that didn't finish leaves fewer clip records for that
    label — indistinguishable, from clip records alone, from "this category
    wasn't scheduled". The status record is the one durable signal that a
    label's partial data means it *failed*, not that it scored a clean 0%;
    last-occurrence-wins so a later clean rerun clears an earlier failure.
    """
    if not path.exists():
        raise SystemExit(f"no results at {path} — run dev/bench.py first")
    latest: dict[tuple[str, str, bool], dict] = {}
    statuses: dict[str, tuple[str, str]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        rec = json.loads(raw)
        if "status" in rec and "clip" not in rec:
            statuses[rec["label"]] = (rec["status"], rec.get("reason", ""))
            continue
        if rec.get("error"):
            # The backend errored instead of transcribing (missing runtime
            # library, unloadable weights). Its empty hypothesis would score as
            # a flawless 100% WER and drag the label's micro-average with it.
            continue
        latest[(rec["label"], rec["clip"], bool(rec.get("cold", False)))] = rec
    return list(latest.values()), statuses


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    return s[min(len(s) - 1, int(q * len(s)))]


def summarize(records: list[dict]) -> dict[str, dict]:
    """Group records by label and micro-average the metrics.

    Accuracy and warm latency come from the warm rows; cold-load latency is
    reported separately from the cold samples (``--cold`` bench runs).
    """
    groups: dict[str, list[dict]] = {}
    for rec in records:
        groups.setdefault(rec["label"], []).append(rec)

    summary = {}
    for label, recs in groups.items():
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
        summary[label] = {
            "clips": len(warm),
            # str(): provenance is written by whatever produced the records, and
            # a structured value here used to kill the whole table at print time
            # - after the sweep that earned it had already run.
            "machine": next(
                (
                    str(r["provenance"]["machine"])
                    for r in recs
                    if isinstance(r.get("provenance"), dict) and r["provenance"].get("machine")
                ),
                None,
            ),
            "wer": wer_edits / ref_words if ref_words else 0.0,
            "cer": cer_edits / ref_chars if ref_chars else 0.0,
            "rtf": _pct(rtfs, 0.5),
            "median_final": _pct(finals, 0.5),
            "p95_final": _pct(finals, 0.95),
            # cold-load = model residency wait only (time_to_ready), from --cold
            # samples; the warm reload should be ~0.
            "cold_ready": max(cold_readys) if cold_readys else None,
            "warm_ready": _pct(warm_readys, 0.5),
            "audio": sum(r["audio_seconds"] for r in warm),
        }
    return summary


def _f(x, spec="6.2f"):
    return format(x, spec) if isinstance(x, (int, float)) else "    --"


def _speed(rtf) -> str:
    """Format RTF as a human-readable speed multiplier.

    0.018 → '55x', 0.046 → '22x', 1.682 → '0.6x'.  Values >= 10x are
    shown as integers; below that one decimal keeps enough resolution to
    distinguish e.g. 5.2x from 4.8x.
    """
    if not isinstance(rtf, (int, float)) or rtf <= 0:
        return "   --"
    x = 1.0 / rtf
    return f"{x:.0f}x" if x >= 10 else f"{x:.1f}x"


def load_resources(path: Path) -> dict[str, dict]:
    """Read the matrix runner's peak RAM/VRAM sidecar (label -> peaks), last
    occurrence winning. Absent file -> empty (resource columns are then hidden).
    """
    if not path.exists():
        return {}
    peaks: dict[str, dict] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw:
            rec = json.loads(raw)
            peaks[rec["label"]] = rec
    return peaks


# Ranking field per --sort choice. All of these are "lower is better" metrics
# (error rate, RTF, latency), so ascending sort puts the best performer first
# uniformly — no special-casing a "higher is better" field.
RANK_FIELDS = {
    "wer": "wer",
    "cer": "cer",
    "speed": "rtf",
    "latency": "median_final",
    "cold-load": "cold_ready",
}


def ranked_labels(
    summary: dict[str, dict], sort: str, statuses: dict[str, tuple[str, str]]
) -> list[str]:
    """Labels ordered best-first by ``sort``; missing values sort last.

    A label whose last recorded status is not "ok" (usability_fail or
    broken) sinks below every clean completion regardless of metric value —
    its WER/speed was measured on however many clips it got through before
    failing, not the full sweep, so it is not a comparable data point and
    must never rank as if it beat a target that actually finished.
    """
    failed = lambda label: statuses.get(label, ("ok", ""))[0] != "ok"  # noqa: E731
    if sort == "label":
        return sorted(summary, key=lambda label: (failed(label), label))
    field = RANK_FIELDS[sort]
    return sorted(
        summary,
        key=lambda label: (
            failed(label),
            summary[label].get(field) is None,
            summary[label].get(field) or 0.0,
            label,
        ),
    )


def print_overall(
    summary: dict[str, dict], order: list[str], statuses: dict[str, tuple[str, str]]
) -> None:
    show_machine = any(s.get("machine") for s in summary.values())
    show_res = any(s.get("peak_rss_mb") for s in summary.values())
    # Sized to the data, not to a guess: labels grew from "cpu/small" to
    # "whisper/cpu/base/streaming" when the runner started reporting the engine,
    # model and mode it actually measured, and a fixed 20 sheared every column
    # to the right of it.
    lw = max(len("label"), *(len(label) for label in summary)) if summary else len("label")
    mh = f"{'machine':14} " if show_machine else ""
    rh = f"{'RSS MB':>9} {'VRAM MB':>9}" if show_res else ""
    print(
        f"{'#':>3} {'label':{lw}} {'status':>13} {mh}{'clips':>5} "
        f"{'WER%':>7} {'CER%':>7} {'speed':>6} "
        f"{'med final':>10} {'p95 final':>10} {'cold load':>10} {rh}"
    )
    print("-" * (lw + 86 + (15 if show_machine else 0) + (20 if show_res else 0)))
    for rank, label in enumerate(order, start=1):
        s = summary[label]
        status, reason = statuses.get(label, ("--", ""))
        status_col = status.upper() if status != "--" else "--"
        if reason:
            status_col = f"{status_col} ({reason[:20]})"
        # Truncated: the column is fixed width, and one long value would shear
        # every other column out of alignment for the whole table.
        machine = (s.get("machine") or "--")[:14]
        mc = f"{machine:14} " if show_machine else ""
        rc = (
            f"{_f(s.get('peak_rss_mb'), '9.1f')} {_f(s.get('peak_vram_mb'), '9.1f')}"
            if show_res
            else ""
        )
        print(
            f"{rank:>3} {label:{lw}} {status_col:>13} {mc}{s['clips']:>5} "
            f"{_f(s['wer'] * 100, '7.2f')} {_f(s['cer'] * 100, '7.2f')} "
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
        " are partial, not comparable); BROKEN = crashed; -- = no matrix.py status record"
        " (e.g. bench.py run directly). Failed/broken rows always sort last regardless of --sort."
    )
    if show_res:
        print("RSS/VRAM = peak memory during the run (matrix runner, server provision).")


def print_by_category(
    records: list[dict], order: list[str], statuses: dict[str, tuple[str, str]]
) -> None:
    records = [r for r in records if not r.get("cold", False)]  # warm only
    labels = [lbl for lbl in order if lbl in {r["label"] for r in records}]
    cats = sorted({r["category"] for r in records})
    # micro WER per (label, category)
    cell: dict[tuple[str, str], tuple[int, int]] = {}
    for r in records:
        key = (r["label"], r["category"])
        e, w = cell.get(key, (0, 0))
        cell[key] = (e + r["wer_edits"], w + r["ref_words"])

    # Labels on Y, categories on X. Column width from the widest category name.
    lw = max(len("label"), *(len(lbl) for lbl in labels)) if labels else len("label")
    cw = max(6, *(len(c) for c in cats)) if cats else 6
    print("\nWER% by category ('--' = no clips scored in that category, not a 0% pass)")
    header = f"{'label':{lw}} " + " ".join(f"{cat:>{cw}}" for cat in cats)
    print(header)
    print("-" * len(header))
    for lbl in labels:
        status, _ = statuses.get(lbl, ("--", ""))
        cells = []
        for cat in cats:
            e, w = cell.get((lbl, cat), (0, 0))
            cells.append(f"{'--':>{cw}}" if not w else f"{e / w * 100:>{cw}.1f}")
        marker = f" [{status.upper()}]" if status not in ("ok", "--") else ""
        print(f"{lbl:{lw}} " + " ".join(cells) + marker)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--in", dest="infile", type=Path, default=REPO_ROOT / "results" / "bench.jsonl"
    )
    parser.add_argument(
        "--by-category",
        action="store_true",
        help="also break WER down by UD129 category",
    )
    parser.add_argument(
        "--sort",
        choices=(*RANK_FIELDS, "label"),
        default="wer",
        help="rank rows best-first by this metric (default: wer); 'label' for alphabetical",
    )
    args = parser.parse_args()

    records, statuses = load_latest(args.infile)
    summary = summarize(records)
    resources = load_resources(args.infile.parent / "matrix-resources.jsonl")
    for label, peaks in resources.items():
        if label in summary:
            summary[label]["peak_rss_mb"] = peaks.get("peak_rss_mb")
            summary[label]["peak_vram_mb"] = peaks.get("peak_vram_mb")
    print(f"{len(records)} records across {len(summary)} label(s) from {args.infile}\n")
    order = ranked_labels(summary, args.sort, statuses)
    print_overall(summary, order, statuses)
    if args.by_category:
        print_by_category(records, order, statuses)


if __name__ == "__main__":
    main()
