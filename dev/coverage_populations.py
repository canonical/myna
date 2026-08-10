#!/usr/bin/env python3
"""Coverage populations + dead-code report (feature 006, FR-005/FR-006).

Classifies every coverable line per language into three populations:
  - test-covered:      hit by the test suite
  - use-case-only:     hit only by the scripted use-case exercise
  - never-executed:    in neither population

Inputs (Cobertura exports, produced by the `cov` / `py-cov` / `exercise`
Workshop actions):
  client/target/coverage/rust-tests.cobertura.xml   (tests only)
  client/target/coverage/rust-merged.cobertura.xml  (tests + use-cases)
  server/coverage-tests.cobertura.xml               (tests only)
  server/coverage-merged.cobertura.xml              (tests + use-cases)

Outputs:
  client/target/coverage/populations.md    human report incl. dead-code section
  client/target/coverage/populations.json  machine-readable classification

Fail-loud (FR-005): any missing input export aborts non-zero naming the file.
The report itself is advisory: findings never fail the run.
"""

from __future__ import annotations

import json
import subprocess
import sys
from collections.abc import Iterable
from pathlib import Path

from coverage_lib import REPO_ROOT, all_lines, covered_lines, parse_cobertura

RUST_TESTS = REPO_ROOT / "client/target/coverage/rust-tests.cobertura.xml"
RUST_MERGED = REPO_ROOT / "client/target/coverage/rust-merged.cobertura.xml"
PY_TESTS = REPO_ROOT / "server/coverage-tests.cobertura.xml"
PY_MERGED = REPO_ROOT / "server/coverage-merged.cobertura.xml"
OUT_MD = REPO_ROOT / "client/target/coverage/populations.md"
OUT_JSON = REPO_ROOT / "client/target/coverage/populations.json"


def component_of(path: str) -> str:
    parts = path.split("/")
    if parts[0] == "client" and len(parts) > 1:
        return f"client/{parts[1]}"
    if parts[0] == "server" and len(parts) > 2:
        return f"server/{parts[2]}"
    return parts[0]


def classify(
    tests_xml: Path, merged_xml: Path
) -> tuple[dict[str, dict[str, int]], dict[str, list[int]]]:
    """Per-component population counts + never-executed lines per file."""
    tests = parse_cobertura(tests_xml)
    merged = parse_cobertura(merged_xml)
    test_cov = covered_lines(tests)
    merged_cov = covered_lines(merged)
    universe = all_lines(merged) | all_lines(tests)

    counts: dict[str, dict[str, int]] = {}
    dead: dict[str, list[int]] = {}
    for f, n in sorted(universe):
        comp = component_of(f)
        bucket = counts.setdefault(
            comp, {"test_covered": 0, "usecase_only": 0, "never_executed": 0}
        )
        if (f, n) in test_cov:
            bucket["test_covered"] += 1
        elif (f, n) in merged_cov:
            bucket["usecase_only"] += 1
        else:
            bucket["never_executed"] += 1
            dead.setdefault(f, []).append(n)
    return counts, dead


def run_statics() -> list[str]:
    """Static dead-code findings (best-effort; each line self-labels its tool)."""
    findings: list[str] = []

    def collect(label: str, cmd: list[str], cwd: Path) -> None:
        try:
            proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=300)
        except FileNotFoundError:
            findings.append(f"- ({label}) tool not installed: `{cmd[0]}`")
            return
        out = (proc.stdout + proc.stderr).strip()
        if proc.returncode == 0 and not out:
            findings.append(f"- ({label}) clean")
        else:
            findings.append(f"```\n$ {' '.join(cmd)}\n{out}\n```")

    collect("cargo machete", ["cargo", "machete"], REPO_ROOT / "client")
    collect(
        "vulture",
        [
            "uv",
            "run",
            "vulture",
            "src",
            "tests",
            "../dev",
            "--min-confidence",
            "80",
        ],
        REPO_ROOT / "server",
    )
    collect(
        "ruff F401/F841",
        ["uv", "run", "ruff", "check", "--select", "F401,F841", "src", "../dev"],
        REPO_ROOT / "server",
    )
    return findings


def render(
    langs: dict[str, dict[str, dict[str, int]]],
    dead: dict[str, list[int]],
    statics: Iterable[str],
) -> str:
    lines = ["# Coverage populations and dead-code report", ""]
    for lang, counts in langs.items():
        lines.append(f"## {lang}")
        lines.append("")
        lines.append("| component | test-covered | use-case-only | never-executed |")
        lines.append("|---|---|---|---|")
        for comp in sorted(counts):
            c = counts[comp]
            lines.append(
                f"| {comp} | {c['test_covered']} | {c['usecase_only']} | {c['never_executed']} |"
            )
        lines.append("")
    lines.append("## Never-executed lines (dynamic dead code)")
    lines.append("")
    if dead:
        for f in sorted(dead):
            nums = dead[f]
            shown = ", ".join(map(str, nums[:25]))
            more = f" … (+{len(nums) - 25} more)" if len(nums) > 25 else ""
            lines.append(f"- `{f}`: {nums and shown}{more}")
    else:
        lines.append("- none")
    lines.append("")
    lines.append("## Static findings (unreferenced items)")
    lines.append("")
    lines.extend(statics)
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    missing = [str(p) for p in (RUST_TESTS, RUST_MERGED, PY_TESTS, PY_MERGED) if not p.exists()]
    if missing:
        print("fail-loud merge: missing coverage export(s):", file=sys.stderr)
        for m in missing:
            print(f"  - {m}", file=sys.stderr)
        print("run `workshop run myna cov py-cov exercise` first", file=sys.stderr)
        return 1

    rust_counts, rust_dead = classify(RUST_TESTS, RUST_MERGED)
    py_counts, py_dead = classify(PY_TESTS, PY_MERGED)
    dead = {**rust_dead, **py_dead}
    langs = {"Rust": rust_counts, "Python": py_counts}

    statics = run_statics()
    OUT_MD.parent.mkdir(parents=True, exist_ok=True)
    OUT_MD.write_text(render(langs, dead, statics))
    OUT_JSON.write_text(
        json.dumps(
            {
                "populations": langs,
                "never_executed": {f: ns for f, ns in sorted(dead.items())},
            },
            indent=2,
        )
    )
    print(f"wrote {OUT_MD}")
    print(f"wrote {OUT_JSON}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
