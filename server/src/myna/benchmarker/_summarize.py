"""Aggregate table for the ``summarize`` subcommand.

Extracted from dev/aggregate.py so it is available inside the zipapp without
a repo checkout. The record schema and output format are identical so results
from the benchmarker and from dev/matrix.py can both be summarised here.
"""

from __future__ import annotations

import json
from pathlib import Path


def _load_latest(path: Path) -> list[dict]:
    """One record per (label, clip, cold), last occurrence winning.

    Skips the machine-header record and error records.
    """
    if not path.exists():
        raise SystemExit(f"no results at {path}")
    latest: dict[tuple[str, str, bool], dict] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        rec = json.loads(raw)
        if rec.get("type") == "machine":
            continue
        if rec.get("error"):
            continue
        latest[(rec["label"], rec["clip"], bool(rec.get("cold", False)))] = rec
    return list(latest.values())


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    return s[min(len(s) - 1, int(q * len(s)))]


def _summarize(records: list[dict]) -> dict[str, dict]:
    groups: dict[str, list[dict]] = {}
    for rec in records:
        groups.setdefault(rec["label"], []).append(rec)

    summary = {}
    for label, recs in groups.items():
        warm = [r for r in recs if not r.get("cold", False)]
        cold = [r for r in recs if r.get("cold", False)]
        finals = [r["finalize_latency"] for r in warm if r.get("finalize_latency") is not None]
        cold_readys = [r["time_to_ready"] for r in cold if r.get("time_to_ready") is not None]
        warm_readys = [r["time_to_ready"] for r in warm if r.get("time_to_ready") is not None]
        rtfs = [r["rtf"] for r in warm if r.get("rtf") is not None]
        wer_edits = sum(r["wer_edits"] for r in warm)
        ref_words = sum(r["ref_words"] for r in warm)
        cer_edits = sum(r["cer_edits"] for r in warm)
        ref_chars = sum(r["ref_chars"] for r in warm)
        summary[label] = {
            "clips": len(warm),
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
            "cold_ready": max(cold_readys) if cold_readys else None,
            "warm_ready": _pct(warm_readys, 0.5),
            "audio": sum(r["audio_seconds"] for r in warm),
            "peak_rss_mb": None,
            "peak_vram_mb": None,
        }
    return summary


def _load_resources(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    peaks: dict[str, dict] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw:
            rec = json.loads(raw)
            peaks[rec["label"]] = rec
    return peaks


def _f(x, spec: str = "6.2f") -> str:
    return format(x, spec) if isinstance(x, (int, float)) else "    --"


def _speed(rtf) -> str:
    if not isinstance(rtf, (int, float)) or rtf <= 0:
        return "   --"
    x = 1.0 / rtf
    return f"{x:.0f}x" if x >= 10 else f"{x:.1f}x"


def _print_overall(summary: dict[str, dict]) -> None:
    show_machine = any(s.get("machine") for s in summary.values())
    show_res = any(s.get("peak_rss_mb") for s in summary.values())
    lw = max(len("label"), *(len(label) for label in summary)) if summary else len("label")
    mh = f"{'machine':14} " if show_machine else ""
    rh = f"{'RSS MB':>9} {'VRAM MB':>9}" if show_res else ""
    print(
        f"{'label':{lw}} {mh}{'clips':>5} {'WER%':>7} {'CER%':>7} {'speed':>6} "
        f"{'med final':>10} {'p95 final':>10} {'cold load':>10} {rh}"
    )
    print("-" * (lw + 68 + (15 if show_machine else 0) + (20 if show_res else 0)))
    for label in sorted(summary):
        s = summary[label]
        machine = (s.get("machine") or "--")[:14]
        mc = f"{machine:14} " if show_machine else ""
        rc = (
            f"{_f(s.get('peak_rss_mb'), '9.1f')} {_f(s.get('peak_vram_mb'), '9.1f')}"
            if show_res
            else ""
        )
        print(
            f"{label:{lw}} {mc}{s['clips']:>5} "
            f"{_f(s['wer'] * 100, '7.2f')} {_f(s['cer'] * 100, '7.2f')} "
            f"{_speed(s['rtf']):>6} "
            f"{_f(s['median_final'], '10.3f')} {_f(s['p95_final'], '10.3f')} "
            f"{_f(s['cold_ready'], '10.3f')} {rc}"
        )
    print(
        "\nmed/p95 final = seconds (end-of-audio -> committed text); "
        "speed = audio/decode (higher is faster)."
    )
    print("cold load = model residency wait (session open -> ready), from --cold runs.")
    if show_res:
        print("RSS/VRAM = peak memory during the run.")


def _print_by_category(records: list[dict]) -> None:
    records = [r for r in records if not r.get("cold", False)]
    labels = sorted({r["label"] for r in records})
    cats = sorted({r["category"] for r in records})
    cell: dict[tuple[str, str], tuple[int, int]] = {}
    for r in records:
        key = (r["label"], r["category"])
        e, w = cell.get(key, (0, 0))
        cell[key] = (e + r["wer_edits"], w + r["ref_words"])
    lw = max(len("label"), *(len(lbl) for lbl in labels)) if labels else len("label")
    cw = max(6, *(len(c) for c in cats)) if cats else 6
    print("\nWER% by category")
    header = f"{'label':{lw}} " + " ".join(f"{cat:>{cw}}" for cat in cats)
    print(header)
    print("-" * len(header))
    for lbl in labels:
        cells = []
        for cat in cats:
            e, w = cell.get((lbl, cat), (0, 0))
            cells.append(f"{(e / w * 100) if w else 0.0:>{cw}.1f}")
        print(f"{lbl:{lw}} " + " ".join(cells))


def cmd_summarize(args) -> None:  # noqa: ANN001
    infile = Path(args.infile)
    records = _load_latest(infile)
    summary = _summarize(records)
    resources = _load_resources(infile.parent / (infile.stem + "-resources.jsonl"))
    for label, peaks in resources.items():
        if label in summary:
            summary[label]["peak_rss_mb"] = peaks.get("peak_rss_mb")
            summary[label]["peak_vram_mb"] = peaks.get("peak_vram_mb")
    print(f"{len(records)} records across {len(summary)} label(s) from {infile}\n")
    _print_overall(summary)
    if getattr(args, "by_category", False):
        _print_by_category(records)
