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
  stdout                                          digest (the debt headline)
  client/target/coverage/populations.md           full human report
  client/target/coverage/populations-summary.md   digest, for a CI job summary
  client/target/coverage/populations.json         machine-readable

Fail-loud (FR-005): any missing input export aborts non-zero naming the file.
The report itself is advisory: findings never fail the run.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from coverage_lib import (
    REPO_ROOT,
    LineHits,
    all_lines,
    covered_lines,
    normalize_path,
    parse_cobertura,
)

RUST_TESTS = REPO_ROOT / "client/target/coverage/rust-tests.cobertura.xml"
RUST_MERGED = REPO_ROOT / "client/target/coverage/rust-merged.cobertura.xml"
PY_TESTS = REPO_ROOT / "server/coverage-tests.cobertura.xml"
PY_MERGED = REPO_ROOT / "server/coverage-merged.cobertura.xml"
OUT_DIR = REPO_ROOT / "client/target/coverage"
OUT_MD = OUT_DIR / "populations.md"
OUT_SUMMARY = OUT_DIR / "populations-summary.md"
OUT_JSON = OUT_DIR / "populations.json"

POPULATIONS = ("test_covered", "usecase_only", "never_executed")
POPULATION_LABELS = {
    "test_covered": "test-covered",
    "usecase_only": "use-case-only",
    "never_executed": "never-executed",
}


# --------------------------------------------------------------------------
# classification


def component_of(path: str) -> str:
    """The ownership bucket a source file rolls up into."""
    parts = path.split("/")
    if parts[0] == "client" and len(parts) > 1:
        return f"client/{parts[1]}"
    # server/src/myna/<subpackage>/... — one bucket per subpackage, because
    # "server/myna" alone hides which half of the tree carries the debt.
    if parts[:3] == ["server", "src", "myna"]:
        return f"server/myna/{parts[3]}" if len(parts) > 4 else "server/myna"
    return parts[0]


def empty_counts() -> dict[str, int]:
    return dict.fromkeys(POPULATIONS, 0)


class Population:
    """One language's line classification, split by component and by file."""

    def __init__(self, tests: LineHits, merged: LineHits) -> None:
        test_cov = covered_lines(tests)
        merged_cov = covered_lines(merged)
        universe = all_lines(merged) | all_lines(tests)

        # A file the exports still name but the tree no longer has means the
        # exports predate a deletion: counting it would report debt that is
        # already gone. Excluded, and surfaced as a staleness warning.
        present: dict[str, bool] = {}
        self.stale: list[str] = []
        self.components: dict[str, dict[str, int]] = {}
        self.file_counts: dict[str, dict[str, int]] = {}
        self.dead: dict[str, list[int]] = {}
        self.covered: set[tuple[str, int]] = test_cov | merged_cov

        for f, n in sorted(universe):
            if f not in present:
                present[f] = (REPO_ROOT / f).exists()
                if not present[f]:
                    self.stale.append(f)
            if not present[f]:
                continue
            comp = self.components.setdefault(component_of(f), empty_counts())
            per_file = self.file_counts.setdefault(f, empty_counts())
            if (f, n) in test_cov:
                key = "test_covered"
            elif (f, n) in merged_cov:
                key = "usecase_only"
            else:
                key = "never_executed"
                self.dead.setdefault(f, []).append(n)
            comp[key] += 1
            per_file[key] += 1

    @property
    def totals(self) -> dict[str, int]:
        out = empty_counts()
        for counts in self.components.values():
            for key in POPULATIONS:
                out[key] += counts[key]
        return out


def coverable(counts: dict[str, int]) -> int:
    return sum(counts[key] for key in POPULATIONS)


def pct(part: int, whole: int) -> str:
    return f"{100.0 * part / whole:.1f}%" if whole else "-"


def line_ranges(nums: Sequence[int]) -> list[tuple[int, int]]:
    """Collapse a sorted line list into contiguous [start, end] ranges."""
    out: list[list[int]] = []
    for n in sorted(nums):
        if out and n == out[-1][1] + 1:
            out[-1][1] = n
        else:
            out.append([n, n])
    return [(a, b) for a, b in out]


def fmt_ranges(nums: Sequence[int], limit: int = 12) -> str:
    spans = line_ranges(nums)
    shown = ", ".join(f"{a}" if a == b else f"{a}-{b}" for a, b in spans[:limit])
    if len(spans) > limit:
        shown += f", … (+{len(spans) - limit} more spans)"
    return shown


# --------------------------------------------------------------------------
# never-entered functions
#
# Line counts say how much is unexecuted; function names say what to go look
# at. The two toolchains expose different handles, so each gets its own.

_CLOSURE = re.compile(r"::\{closure#\d+\}")


def _strip_generics(name: str) -> str:
    """Drop `::<...>` monomorphization suffixes from an llvm-cov symbol."""
    out: list[str] = []
    depth = 0
    i = 0
    while i < len(name):
        if name.startswith("::<", i):
            depth += 1
            i += 3
            continue
        char = name[i]
        if depth:
            depth += char == "<"
            depth -= char == ">"
        else:
            out.append(char)
        i += 1
    return "".join(out)


def rust_dead_functions(merged_xml: Path) -> dict[str, list[str]]:
    """Rust functions whose entry line was never hit, keyed by file.

    cargo-llvm-cov's Cobertura lists one `<method>` per monomorphization with
    only the function's entry line, so the instantiations collapse to one name
    and a closure folds into its parent: a cold closure inside a hot function
    is a dead branch, not a dead function, and the line counts already show it.
    """
    hits: dict[tuple[str, str], int] = {}
    for cls in ET.parse(merged_xml).getroot().iter("class"):
        filename = cls.get("filename")
        if not filename:
            continue
        rel = normalize_path(filename)
        for method in cls.iter("method"):
            name = _CLOSURE.sub("", _strip_generics(method.get("name", "")))
            for line in method.iter("line"):
                key = (rel, name)
                hits[key] = max(hits.get(key, 0), int(line.get("hits", "0")))

    dead: dict[str, list[str]] = {}
    for (rel, name), count in sorted(hits.items()):
        if count == 0 and (REPO_ROOT / rel).exists():
            dead.setdefault(rel, []).append(name)
    return dead


def python_dead_functions(pop: Population) -> dict[str, list[str]]:
    """Python defs whose whole body is never-executed, keyed by file.

    coverage.py's Cobertura carries no `<method>` elements, and a `def` line
    runs at import time regardless, so the body is what has to be checked. A
    def nested inside an already-dead def is dropped: it is one finding.
    """
    dead: dict[str, list[str]] = {}
    for rel, lines in sorted(pop.dead.items()):
        source = REPO_ROOT / rel
        if source.suffix != ".py":
            continue
        try:
            tree = ast.parse(source.read_text())
        except (OSError, SyntaxError):
            continue
        found: list[tuple[int, int, str]] = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                continue
            start, end = node.body[0].lineno, node.end_lineno or node.body[0].lineno
            body = [n for n in lines if start <= n <= end]
            if not body:
                continue
            if any((rel, n) in pop.covered for n in range(start, end + 1)):
                continue
            found.append((start, end, node.name))
        outer = [
            (start, end, name)
            for start, end, name in found
            if not any(o_s < start and end <= o_e for o_s, o_e, _ in found)
        ]
        if outer:
            dead[rel] = [f"{name} (L{start})" for start, end, name in sorted(outer)]
    return dead


# --------------------------------------------------------------------------
# static findings


class Static:
    """One static dead-code tool's outcome."""

    def __init__(self, label: str, command: list[str], output: str, status: str) -> None:
        self.label = label
        self.command = command
        self.output = output
        self.status = status  # "clean" | "findings" | "skipped"

    @property
    def verdict(self) -> str:
        if self.status == "clean":
            return "clean"
        if self.status == "skipped":
            return f"not run ({self.output})"
        return f"{len([ln for ln in self.output.splitlines() if ln.strip()])} line(s) of findings"


def run_statics() -> list[Static]:
    """Static dead-code findings. A tool that is absent is reported as absent:
    a check that did not run must never read as a check that passed."""
    checks = [
        ("cargo machete", ["cargo", "machete"], REPO_ROOT / "client", "cargo-machete"),
        (
            "vulture",
            ["uv", "run", "vulture", "src", "tests", "../dev", "--min-confidence", "80"],
            REPO_ROOT / "server",
            "uv",
        ),
        (
            "ruff F401/F841",
            ["uv", "run", "ruff", "check", "--select", "F401,F841", "src", "../dev"],
            REPO_ROOT / "server",
            "uv",
        ),
    ]
    results: list[Static] = []
    for label, cmd, cwd, binary in checks:
        if shutil.which(binary) is None:
            results.append(Static(label, cmd, f"`{binary}` not installed", "skipped"))
            continue
        try:
            proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=300)
        except (OSError, subprocess.SubprocessError) as exc:
            results.append(Static(label, cmd, str(exc), "skipped"))
            continue
        # A zero exit is the tool's own verdict; its chatter on stdout ("Good
        # job!") is not a finding.
        output = (proc.stdout + proc.stderr).strip()
        if proc.returncode == 0:
            results.append(Static(label, cmd, "", "clean"))
        else:
            results.append(Static(label, cmd, output, "findings"))
    return results


# --------------------------------------------------------------------------
# rendering


def table(header: Sequence[str], rows: Sequence[Sequence[str]], markdown: bool) -> list[str]:
    """One row set, rendered as a markdown table or as aligned plain text."""
    if markdown:
        return [
            "| " + " | ".join(header) + " |",
            "|" + "|".join("---" for _ in header) + "|",
            *["| " + " | ".join(r) + " |" for r in rows],
        ]
    width = [max(len(str(c)) for c in col) for col in zip(header, *rows, strict=True)]
    out = []
    for i, row in enumerate([header, *rows]):
        cells = [
            str(c).ljust(width[j]) if j == 0 else str(c).rjust(width[j]) for j, c in enumerate(row)
        ]
        out.append("  " + "  ".join(cells).rstrip())
        if i == 0:
            out.append("  " + "  ".join("-" * w for w in width))
    return out


def population_rows(langs: dict[str, Population]) -> tuple[list[str], list[list[str]]]:
    totals = {lang: pop.totals for lang, pop in langs.items()}
    grand = empty_counts()
    for counts in totals.values():
        for key in POPULATIONS:
            grand[key] += counts[key]
    columns = [*totals.items(), ("total", grand)]

    header = ["population", *[name for name, _ in columns]]
    rows = [["coverable", *[str(coverable(c)) for _, c in columns]]]
    for key in POPULATIONS:
        rows.append(
            [
                POPULATION_LABELS[key],
                *[f"{c[key]} ({pct(c[key], coverable(c))})" for _, c in columns],
            ]
        )
    return header, rows


def component_rows(langs: dict[str, Population]) -> list[list[str]]:
    rows = []
    for lang, pop in langs.items():
        for comp in sorted(pop.components, key=lambda c: -pop.components[c]["never_executed"]):
            counts = pop.components[comp]
            total = coverable(counts)
            rows.append(
                [
                    comp,
                    lang,
                    str(total),
                    f"{counts['test_covered']} ({pct(counts['test_covered'], total)})",
                    f"{counts['usecase_only']} ({pct(counts['usecase_only'], total)})",
                    f"{counts['never_executed']} ({pct(counts['never_executed'], total)})",
                ]
            )
    rows.sort(key=lambda r: -int(r[5].split(" ")[0]))
    return rows


def hotspot_rows(langs: dict[str, Population], dead_fns: dict[str, list[str]]) -> list[list[str]]:
    rows = []
    for pop in langs.values():
        for f, counts in pop.file_counts.items():
            if not counts["never_executed"]:
                continue
            total = coverable(counts)
            rows.append(
                [
                    f,
                    str(counts["never_executed"]),
                    pct(counts["never_executed"], total),
                    str(len(dead_fns.get(f, []))),
                ]
            )
    rows.sort(key=lambda r: -int(r[1]))
    return rows


@dataclass
class Report:
    """Everything one run measured, assembled once and rendered three ways."""

    langs: dict[str, Population]
    dead_fns: dict[str, list[str]]
    statics: list[Static]
    measured: str
    newer_sources: int
    top: int

    def staleness_warnings(self) -> list[str]:
        """Reasons to distrust these numbers, in the reader's face, not a footnote."""
        warnings = []
        stale = sorted({f for pop in self.langs.values() for f in pop.stale})
        if stale:
            warnings.append(
                f"STALE: {len(stale)} file(s) named by the exports no longer "
                f"exist and were excluded ({', '.join(stale[:3])}"
                f"{', …' if len(stale) > 3 else ''})."
            )
        if self.newer_sources:
            warnings.append(
                f"STALE: {self.newer_sources} tracked source file(s) changed "
                "after these exports were written."
            )
        if warnings:
            warnings.append("Re-run `cov`, `py-cov` and `exercise` for current numbers.")
        return warnings


def digest(report: Report, markdown: bool) -> list[str]:
    """The part of the report worth reading in a terminal or a CI summary."""
    langs = report.langs
    hotspots = hotspot_rows(langs, report.dead_fns)
    fn_count = sum(len(v) for v in report.dead_fns.values())
    lines: list[str] = []

    def para(text: str) -> None:
        lines.extend([text, ""])

    def heading(text: str) -> None:
        para(f"## {text}" if markdown else text)

    if markdown:
        para("# Coverage populations and dead-code report")
        para(f"Measured from coverage exports written {report.measured}.")
    else:
        para(f"Coverage populations and dead-code report ({report.measured})")

    for warning in report.staleness_warnings():
        para(warning)

    header, rows = population_rows(langs)
    lines.extend(table(header, rows, markdown))
    lines.append("")
    para(
        "Never-executed is the debt headline: coverable lines that no test and "
        "no scripted use-case reached. Use-case-only lines are a test gap, not "
        "dead code."
    )

    heading("Debt by component")
    lines.extend(
        table(
            ["component", "lang", "coverable", "test-covered", "use-case-only", "never-executed"],
            component_rows(langs),
            markdown,
        )
    )
    lines.append("")

    heading(f"Never-executed hot spots (top {report.top} of {len(hotspots)} files)")
    lines.extend(
        table(["file", "dead lines", "of file", "dead fns"], hotspots[: report.top], markdown)
    )
    lines.append("")
    para(
        f"Never-entered functions: {fn_count} across {len(report.dead_fns)} files. "
        "A function no run enters is the actionable end of the debt; the full "
        "report names them."
    )

    heading("Static findings")
    if report.statics:
        lines.extend(f"- {static.label}: {static.verdict}" for static in report.statics)
    else:
        lines.append("- skipped (--no-statics)")
    lines.append("")

    para(f"Full report: {OUT_MD.relative_to(REPO_ROOT)}")
    return lines


def render_full(report: Report) -> str:
    langs, dead_fns, statics = report.langs, report.dead_fns, report.statics
    lines = digest(report, markdown=True)

    lines.append("## Never-entered functions")
    lines.append("")
    if dead_fns:
        for f in sorted(dead_fns):
            lines.append(f"- `{f}`")
            lines.extend(f"  - `{name}`" for name in dead_fns[f])
    else:
        lines.append("- none")
    lines.append("")

    lines.append("## Never-executed lines (dynamic dead code)")
    lines.append("")
    any_dead = False
    for pop in langs.values():
        for f in sorted(pop.dead):
            any_dead = True
            lines.append(f"- `{f}`: {fmt_ranges(pop.dead[f])}")
    if not any_dead:
        lines.append("- none")
    lines.append("")

    lines.append("## Static findings (raw)")
    lines.append("")
    for static in statics:
        lines.append(f"### {static.label} — {static.verdict}")
        lines.append("")
        lines.append("```")
        lines.append("$ " + " ".join(static.command))
        lines.append(static.output or "(clean)")
        lines.append("```")
        lines.append("")
    return "\n".join(lines)


def as_json(report: Report) -> dict:
    langs = report.langs
    grand = empty_counts()
    for pop in langs.values():
        for key, value in pop.totals.items():
            grand[key] += value
    return {
        "totals": {**grand, "coverable": coverable(grand)},
        "languages": {
            lang: {
                "totals": {**pop.totals, "coverable": coverable(pop.totals)},
                "components": pop.components,
                "stale_files": pop.stale,
            }
            for lang, pop in langs.items()
        },
        "populations": {lang: pop.components for lang, pop in langs.items()},
        "never_executed": {
            f: nums for pop in langs.values() for f, nums in sorted(pop.dead.items())
        },
        "never_entered_functions": report.dead_fns,
        "stale": {
            "measured": report.measured,
            "sources_changed_since": report.newer_sources,
        },
    }


# --------------------------------------------------------------------------


def measured_at(paths: Iterable[Path]) -> float:
    return min(p.stat().st_mtime for p in paths)


def sources_newer_than(when: float) -> int:
    """Tracked Rust/Python sources modified after the exports were written.

    A deleted file shows up in the export itself; a file added or edited since
    does not, and silently reports as debt that a re-run would clear. On a CI
    checkout every source predates the exports, so this stays quiet there.
    """
    try:
        listing = subprocess.run(
            ["git", "ls-files", "-z", "--", "*.rs", "*.py"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            timeout=60,
            check=True,
        )
    except (OSError, subprocess.SubprocessError):
        return 0
    newer = 0
    for rel in listing.stdout.split("\0"):
        path = REPO_ROOT / rel
        if rel and path.exists() and path.stat().st_mtime > when:
            newer += 1
    return newer


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--top", type=int, default=15, help="hot-spot files to list in the digest (default 15)"
    )
    parser.add_argument(
        "--no-statics", action="store_true", help="skip the static tools (faster; report only)"
    )
    args = parser.parse_args(argv)

    exports = (RUST_TESTS, RUST_MERGED, PY_TESTS, PY_MERGED)
    missing = [str(p) for p in exports if not p.exists()]
    if missing:
        print("fail-loud merge: missing coverage export(s):", file=sys.stderr)
        for m in missing:
            print(f"  - {m}", file=sys.stderr)
        print("run `workshop run myna cov py-cov exercise` first", file=sys.stderr)
        return 1

    langs = {
        "Rust": Population(parse_cobertura(RUST_TESTS), parse_cobertura(RUST_MERGED)),
        "Python": Population(parse_cobertura(PY_TESTS), parse_cobertura(PY_MERGED)),
    }
    measured = measured_at(exports)
    report = Report(
        langs=langs,
        dead_fns={
            **rust_dead_functions(RUST_MERGED),
            **python_dead_functions(langs["Python"]),
        },
        statics=[] if args.no_statics else run_statics(),
        measured=datetime.fromtimestamp(measured).strftime("%Y-%m-%d %H:%M"),
        newer_sources=sources_newer_than(measured),
        top=args.top,
    )

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    OUT_MD.write_text(render_full(report))
    OUT_SUMMARY.write_text("\n".join(digest(report, markdown=True)))
    OUT_JSON.write_text(json.dumps(as_json(report), indent=2))

    print("\n".join(digest(report, markdown=False)))
    print(f"wrote {OUT_MD}")
    print(f"wrote {OUT_SUMMARY}")
    print(f"wrote {OUT_JSON}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
