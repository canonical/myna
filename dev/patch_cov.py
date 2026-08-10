#!/usr/bin/env python3
"""patch-cov — self-hosted patch-coverage gate (feature 006, FR-008).

Diffs the PR against its merge base, restricts the merged Cobertura exports
(Rust + Python) to the changed lines, and enforces a coverage threshold
(default 80%). No data leaves CI; no external service.

Pass conditions (any):
  - covered-changed / coverable-changed >= threshold
  - zero coverable changed lines (e.g. deletion-only PRs)
  - fewer than FLOOR coverable changed lines (rounding-noise guard)

Exit: 0 pass; 2 below threshold; 1 tool error (never fail-open).

Usage: dev/patch_cov.py [--base <ref>] [--fail-under <pct>]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from coverage_lib import REPO_ROOT, all_lines, covered_lines, parse_cobertura

EXPORTS = [
    REPO_ROOT / "client/target/coverage/rust-merged.cobertura.xml",
    REPO_ROOT / "server/coverage-merged.cobertura.xml",
]

# Exclusion patterns (repo-root-relative prefixes/globs): generated, vendored,
# snapshots, non-code. One place, per the ci-gates contract.
EXCLUDE_PREFIXES = (
    "specs/",
    "docs/",
    "target/",
    ".github/",
    "server/tests/fixtures/",
    "corpus/",
    "server/uv.lock",
    "client/Cargo.lock",
)

FLOOR = 5  # coverable-line floor: smaller diffs pass unconditionally
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def changed_lines(base: str) -> dict[str, set[int]]:
    """Added/changed lines per file (repo-root-relative) vs the merge base."""
    proc = subprocess.run(
        ["git", "diff", "--unified=0", f"{base}...HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"git diff failed: {proc.stderr.strip()}")
    result: dict[str, set[int]] = {}
    current: str | None = None
    for line in proc.stdout.splitlines():
        if line.startswith("+++ b/"):
            current = line[len("+++ b/") :]
        elif line.startswith("+++"):
            current = None  # /dev/null (deleted file)
        elif current and (m := HUNK_RE.match(line)):
            start = int(m.group(1))
            count = int(m.group(2) or "1")
            if count == 0:
                continue  # pure deletion hunk: no added lines
            result.setdefault(current, set()).update(range(start, start + count))
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="origin/main")
    ap.add_argument("--fail-under", type=float, default=80.0)
    args = ap.parse_args()

    missing = [str(p) for p in EXPORTS if not p.exists()]
    if missing:
        print("patch-cov: missing coverage export(s):", file=sys.stderr)
        for m in missing:
            print(f"  - {m}", file=sys.stderr)
        print("run the cov/py-cov/exercise actions first", file=sys.stderr)
        return 1

    try:
        changes = changed_lines(args.base)
    except RuntimeError as e:
        print(f"patch-cov: {e}", file=sys.stderr)
        return 1

    # Coverable lines from both exports.
    universe: set[tuple[str, int]] = set()
    covered: set[tuple[str, int]] = set()
    for xml in EXPORTS:
        hits = parse_cobertura(xml)
        universe |= all_lines(hits)
        covered |= covered_lines(hits)

    def excluded(path: str) -> bool:
        return path.startswith(EXCLUDE_PREFIXES)

    coverable = {
        (f, n)
        for f, lines in changes.items()
        if not excluded(f)
        for n in lines
        if (f, n) in universe
    }
    hit = {(f, n) for f, n in coverable if (f, n) in covered}
    missed = sorted(coverable - hit)

    total = len(coverable)
    pct = 100.0 * len(hit) / total if total else 100.0

    lines = [
        f"patch coverage: {len(hit)}/{total} changed coverable lines covered ({pct:.1f}%)",
        f"threshold: {args.fail_under:.0f}%  floor: {FLOOR} lines  base: {args.base}",
    ]

    out_dir = REPO_ROOT / "client/target/coverage"
    out_dir.mkdir(parents=True, exist_ok=True)
    html = out_dir / "patch-coverage.html"
    with tempfile.NamedTemporaryFile("w", suffix=".diff", delete=False) as tf:
        subprocess.run(
            ["git", "diff", f"{args.base}...HEAD"], cwd=REPO_ROOT, stdout=tf, check=False
        )
        diff_path = tf.name
    # diff-cover lives next to the interpreter when run via the server venv.
    diff_cover = Path(sys.executable).parent / "diff-cover"
    if not diff_cover.exists():
        diff_cover = Path("diff-cover")
    try:
        subprocess.run(
            [
                str(diff_cover),
                *(str(x) for x in EXPORTS),
                f"--diff-file={diff_path}",
                f"--html-report={html}",
            ],
            cwd=REPO_ROOT,
            capture_output=True,
            check=False,
        )
    except FileNotFoundError:
        print("patch-cov: diff-cover not installed (uv sync the dev group)", file=sys.stderr)
        return 1
    finally:
        Path(diff_path).unlink(missing_ok=True)

    verdict_ok = total < FLOOR or pct >= args.fail_under
    if total == 0:
        lines.append("verdict: PASS (no coverable changed lines)")
    elif total < FLOOR:
        lines.append(f"verdict: PASS (below {FLOOR}-line floor)")
    elif verdict_ok:
        lines.append("verdict: PASS")
    else:
        lines.append("verdict: FAIL — uncovered changed lines:")
        by_file: dict[str, list[int]] = {}
        for f, n in missed:
            by_file.setdefault(f, []).append(n)
        for f in sorted(by_file):
            for n in by_file[f]:
                lines.append(f"  {f}:{n}")
    lines.append(f"report: {html}")
    print("\n".join(lines))
    return 0 if verdict_ok else 2


if __name__ == "__main__":
    raise SystemExit(main())
