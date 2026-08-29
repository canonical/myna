#!/usr/bin/env python3
"""GJS coverage digest (feature 006, GJS extension suites).

GJS emits lcov-format coverage when run with
`--coverage-prefix=PREFIX --coverage-output=DIR`. Across several `gjs` runs
that output lands in one lcov file per run; this tool merges a directory of
them (or a single file), keeps only the records whose source path lives under
the extension tree, and reports line / branch totals and per-file detail.

Inputs:
  <lcov>            merged lcov (or a directory of coverage.lcov files) from gjs
  <source-root>     repo path the counted files must live under (e.g.
                    extensions/myna-shell); anything else (gjs internals,
                    imported system modules) is excluded from the count.

Outputs:
  stdout            digest (totals + per-file lines) - CI-friendly
  <out>/gjs-summary.md     the same digest, for a CI job summary
  <out>/gjs-summary.json   machine-readable totals + per-file
  <out>/gjs-extension.lcov lcov restricted to the extension, for genhtml later

Fail-loud: a missing or empty input aborts non-zero naming the cause. The
report itself is advisory - findings never fail the run.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Record:
    source: str
    lf: int
    lh: int
    brf: int
    brh: int


def physical_lines(path: Path) -> int:
    """Non-blank, non-comment source lines - a denominator for files gjs could
    not instrument (e.g. Shell-bound modules), so they still weigh the total."""
    count = 0
    in_block = False
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if in_block:
            if stripped.endswith("*/"):
                in_block = False
            continue
        if stripped.startswith("/*"):
            in_block = not stripped.endswith("*/")
            continue
        if stripped.startswith("//"):
            continue
        count += 1
    return count


def iter_lcov(path: Path):
    """Yield one lcov record (list of lines) per SF block."""
    text = path.read_text(encoding="utf-8", errors="replace")
    record: list[str] = []
    for line in text.splitlines():
        record.append(line)
        if line == "end_of_record":
            yield record
            record = []
    if record:
        yield record


def parse_record(lines: list[str]) -> Record | None:
    source = None
    lf = lh = brf = brh = 0
    for line in lines:
        if line.startswith("SF:"):
            source = line[len("SF:") :].strip()
        elif line.startswith("LF:"):
            lf = int(line[len("LF:") :])
        elif line.startswith("LH:"):
            lh = int(line[len("LH:") :])
        elif line.startswith("BRF:"):
            brf = int(line[len("BRF:") :])
        elif line.startswith("BRH:"):
            brh = int(line[len("BRH:") :])
    if source is None:
        return None
    return Record(source=source, lf=lf, lh=lh, brf=brf, brh=brh)


def collect_records(lcov_arg: Path) -> list[Record]:
    files: list[Path] = []
    if lcov_arg.is_dir():
        files = sorted(lcov_arg.rglob("*.lcov")) or sorted(lcov_arg.rglob("coverage.lcov"))
    else:
        files = [lcov_arg]
    if not files:
        raise SystemExit(f"gjs_coverage: no lcov input found at {lcov_arg}")
    records: list[Record] = []
    for f in files:
        for rec in iter_lcov(f):
            parsed = parse_record(rec)
            if parsed is not None:
                records.append(parsed)
    if not records:
        raise SystemExit(f"gjs_coverage: no SF records in {lcov_arg}")
    return records


def pct(hit: int, total: int) -> float:
    return (hit / total * 100.0) if total else 0.0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("lcov", type=Path, help="merged lcov file or directory of lcov files")
    ap.add_argument("source_root", type=Path, help="repo path counted files must live under")
    ap.add_argument(
        "--raw",
        type=Path,
        default=None,
        help="the per-run coverage-output dir gjs wrote into; its children are "
        "the run dirs gjs prefixed each copied source with, so a record's real "
        "source is <source_root>/<path-after-the-run-dir>",
    )
    ap.add_argument("--out", type=Path, default=None, help="output dir for summary files")
    args = ap.parse_args()

    root = args.source_root.resolve()
    records = collect_records(args.lcov)

    # gjs copies every loaded source into --coverage-output, prefix-stripped, so
    # the lcov SF paths point at those copies (under <raw>/<run>/), not the tree.
    # Map each copy back to its real source: drop the run dir, re-anchor on
    # <source_root>, and keep only files that actually exist there. That drops
    # gjs internals (org/gnome/gjs/...) and the test harness; the Shell-bound
    # modules (host.js/extension.js) aren't importable headlessly so they simply
    # never appear - an honest "counted for what ran".
    raw = args.raw.resolve() if args.raw else None

    def real_source(rec: Record) -> Path | None:
        src = Path(rec.source).resolve()
        if raw is not None and src.is_relative_to(raw):
            rel = src.relative_to(raw)
            parts = rel.parts[1:]  # drop the per-run dir
            if not parts:
                return None
            cand = root / Path(*parts)
        else:
            cand = src
        if not cand.is_relative_to(root):
            return None
        if not cand.is_file():
            return None
        # Count the shipped modules, not the test files themselves.
        if cand.relative_to(root).parts and cand.relative_to(root).parts[0] == "test":
            return None
        return cand

    mapped = [(r, real_source(r)) for r in records]
    kept = [(r, rs) for r, rs in mapped if rs is not None]
    if not kept:
        raise SystemExit(
            f"gjs_coverage: no extension records under {args.source_root} "
            f"(saw {len(records)} total; prefix/--raw may be wrong)"
        )

    seen = {rs for _, rs in kept}

    # The honest denominator: every shipped module, not just the ones that ran.
    # The Shell-bound files (host.js/extension.js, …) can't be imported without
    # a live Shell, so they never appear in the lcov - list them as 0% rather
    # than hiding the gap. Tests and generated coverage output are not counted.
    zeros: list[tuple[Record, Path]] = []
    for src in sorted(root.rglob("*.js")):
        rel = src.relative_to(root)
        if rel.parts[0] in ("test", "target", "po"):
            continue
        if src in seen:
            continue
        zeros.append((Record(source=str(src), lf=physical_lines(src), lh=0, brf=0, brh=0), src))

    all_records = kept + zeros

    t_lf = sum(r.lf for r, _ in all_records)
    t_lh = sum(r.lh for r, _ in all_records)
    t_brf = sum(r.brf for r, _ in all_records)
    t_brh = sum(r.brh for r, _ in all_records)

    by_file = sorted(
        (
            {
                "source": str(rs.relative_to(root.parent)),
                "lines": r.lf,
                "lines_hit": r.lh,
                "line_pct": round(pct(r.lh, r.lf), 1),
            }
            for r, rs in all_records
            if r.lf > 0 or rs in {z for _, z in zeros}
        ),
        key=lambda d: (d["line_pct"], d["source"]),
    )

    digest = (
        "## GJS extension coverage\n\n"
        f"- **Lines:** {t_lh}/{t_lf} "
        f"({pct(t_lh, t_lf):.1f}%) across {len(by_file)} source files\n"
        f"- **Branches:** {t_brh}/{t_brf} ({pct(t_brh, t_brf):.1f}%)\n\n"
        "| file | lines | hit | % |\n|---|---|---|---|\n"
        + "\n".join(
            f"| {d['source']} | {d['lines']} | {d['lines_hit']} | {d['line_pct']} |"
            for d in by_file
        )
        + "\n"
    )
    print(digest)

    if args.out is not None:
        args.out.mkdir(parents=True, exist_ok=True)
        (args.out / "gjs-summary.md").write_text(digest, encoding="utf-8")
        (args.out / "gjs-summary.json").write_text(
            json.dumps(
                {
                    "lines_total": t_lf,
                    "lines_hit": t_lh,
                    "line_pct": round(pct(t_lh, t_lf), 2),
                    "branches_total": t_brf,
                    "branches_hit": t_brh,
                    "branch_pct": round(pct(t_brh, t_brf), 2),
                    "files": by_file,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        # Restricted lcov for genhtml / downstream merging, re-anchored on the
        # real source tree so genhtml can find the sources.
        with (args.out / "gjs-extension.lcov").open("w", encoding="utf-8") as fh:
            for r, rs in kept:
                fh.write(f"SF:{rs}\n")
                fh.write(f"LF:{r.lf}\n")
                fh.write(f"LH:{r.lh}\n")
                if r.brf:
                    fh.write(f"BRF:{r.brf}\n")
                    fh.write(f"BRH:{r.brh}\n")
                fh.write("end_of_record\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
