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


def load_latest(path: Path) -> list[dict]:
    """Return one record per (label, clip), last occurrence winning."""
    if not path.exists():
        raise SystemExit(f"no results at {path} — run dev/bench.py first")
    latest: dict[tuple[str, str], dict] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        rec = json.loads(raw)
        latest[(rec["label"], rec["clip"])] = rec
    return list(latest.values())


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    return s[min(len(s) - 1, int(q * len(s)))]


def summarize(records: list[dict]) -> dict[str, dict]:
    """Group records by label and micro-average the metrics."""
    groups: dict[str, list[dict]] = {}
    for rec in records:
        groups.setdefault(rec["label"], []).append(rec)

    summary = {}
    for label, recs in groups.items():
        finals = [r["finalize_latency"] for r in recs if r.get("finalize_latency") is not None]
        wer_edits = sum(r["wer_edits"] for r in recs)
        ref_words = sum(r["ref_words"] for r in recs)
        cer_edits = sum(r["cer_edits"] for r in recs)
        ref_chars = sum(r["ref_chars"] for r in recs)
        summary[label] = {
            "clips": len(recs),
            "wer": wer_edits / ref_words if ref_words else 0.0,
            "cer": cer_edits / ref_chars if ref_chars else 0.0,
            "median_final": _pct(finals, 0.5),
            "p90_final": _pct(finals, 0.9),
            "audio": sum(r["audio_seconds"] for r in recs),
        }
    return summary


def _f(x, spec="6.2f"):
    return format(x, spec) if isinstance(x, (int, float)) else "    --"


def print_overall(summary: dict[str, dict]) -> None:
    print(f"{'label':20} {'clips':>5} {'WER%':>7} {'CER%':>7} {'med final':>10} {'p90 final':>10}")
    print("-" * 64)
    for label in sorted(summary):
        s = summary[label]
        print(
            f"{label:20} {s['clips']:>5} "
            f"{_f(s['wer'] * 100, '7.2f')} {_f(s['cer'] * 100, '7.2f')} "
            f"{_f(s['median_final'], '10.3f')} {_f(s['p90_final'], '10.3f')}"
        )


def print_by_category(records: list[dict]) -> None:
    labels = sorted({r["label"] for r in records})
    cats = sorted({r["category"] for r in records})
    # micro WER per (category, label)
    cell: dict[tuple[str, str], tuple[int, int]] = {}
    for r in records:
        e, w = cell.get((r["category"], r["label"]), (0, 0))
        cell[(r["category"], r["label"])] = (e + r["wer_edits"], w + r["ref_words"])

    print("\nWER% by category")
    header = f"{'category':12} " + " ".join(f"{lbl:>16}" for lbl in labels)
    print(header)
    print("-" * len(header))
    for cat in cats:
        row = f"{cat:12} "
        for lbl in labels:
            e, w = cell.get((cat, lbl), (0, 0))
            row += f"{(e / w * 100) if w else 0.0:>16.1f}"
        print(row)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--in", dest="infile", type=Path, default=REPO_ROOT / "results" / "bench.jsonl")
    parser.add_argument("--by-category", action="store_true", help="also break WER down by UD129 category")
    args = parser.parse_args()

    records = load_latest(args.infile)
    summary = summarize(records)
    print(f"{len(records)} records across {len(summary)} label(s) from {args.infile}\n")
    print_overall(summary)
    if args.by_category:
        print_by_category(records)


if __name__ == "__main__":
    main()
