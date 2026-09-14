"""Tests for the GJS coverage digest (dev/gjs_coverage.py).

Each gjs run writes its own lcov, and a module imported by several suites is
recorded once per run. The digest must count that module once, with the union
of what the runs hit, or the extension's number is inflated by duplicates.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "dev"))

import gjs_coverage  # noqa: E402


def run_record(
    raw: Path, run: str, module: str, lines: dict[int, int], branches: list[str]
) -> None:
    """Append one gjs record for `module` to <raw>/<run>/coverage.lcov, with the
    SF pointing at gjs's copy of the source under the run dir, as gjs writes it."""
    run_dir = raw / run
    run_dir.mkdir(parents=True, exist_ok=True)
    body = [f"SF:{run_dir / module}", "FN:1,top-level", f"FNDA:{min(lines.values())},top-level"]
    body += [f"BRDA:{b}" for b in branches]
    body += [
        f"BRF:{len(branches)}",
        f"BRH:{sum(1 for b in branches if b.split(',')[3] not in ('0', '-'))}",
    ]
    body += [f"DA:{n},{h}" for n, h in sorted(lines.items())]
    body += [f"LF:{len(lines)}", f"LH:{sum(1 for h in lines.values() if h)}", "end_of_record"]
    with (run_dir / "coverage.lcov").open("a") as fh:
        fh.write("\n".join(body) + "\n")


@pytest.fixture
def digest(tmp_path, monkeypatch):
    root = tmp_path / "myna-shell"
    root.mkdir()
    raw = root / "target" / "coverage" / "raw"
    out = root / "target" / "coverage"

    def run(modules: list[str]) -> tuple[dict, str]:
        for module in modules:
            (root / module).write_text("export const x = 1;\n")
        monkeypatch.setattr(
            sys,
            "argv",
            ["gjs_coverage.py", str(raw), str(root), "--raw", str(raw), "--out", str(out)],
        )
        assert gjs_coverage.main() == 0
        summary = json.loads((out / "gjs-summary.json").read_text())
        return summary, (out / "gjs-extension.lcov").read_text()

    return raw, run


def test_a_module_recorded_by_two_runs_is_counted_once_with_the_union(digest):
    raw, run = digest
    run_record(raw, "place.test.js", "place.js", {1: 1, 2: 0, 3: 0}, ["2,0,0,0", "2,0,1,1"])
    run_record(raw, "host.test.js", "place.js", {1: 1, 2: 4, 3: 0}, ["2,0,0,-", "2,0,1,0"])

    summary, _ = run(["place.js"])

    assert [f["source"] for f in summary["files"]] == ["myna-shell/place.js"]
    assert (summary["lines_total"], summary["lines_hit"]) == (3, 2)
    assert (summary["branches_total"], summary["branches_hit"]) == (2, 1)


def test_the_merged_lcov_carries_one_record_with_summed_detail(digest):
    raw, run = digest
    run_record(raw, "place.test.js", "place.js", {1: 1, 2: 0}, ["2,0,0,3", "2,0,1,0"])
    run_record(raw, "host.test.js", "place.js", {1: 2, 2: 5}, ["2,0,0,-", "2,0,1,4"])

    _, lcov = run(["place.js"])

    assert lcov.count("SF:") == 1
    for line in [
        "DA:1,3",
        "DA:2,5",
        "BRDA:2,0,0,3",
        "BRDA:2,0,1,4",
        "LF:2",
        "LH:2",
        "BRF:2",
        "BRH:2",
    ]:
        assert line in lcov.splitlines(), line


def test_a_module_recorded_by_one_run_keeps_its_own_numbers(digest):
    raw, run = digest
    run_record(raw, "resolve.test.js", "resolve.js", {1: 1, 2: 0, 3: 2}, ["3,0,0,0", "3,0,1,-"])

    summary, _ = run(["resolve.js"])

    assert summary["files"] == [
        {"source": "myna-shell/resolve.js", "lines": 3, "lines_hit": 2, "line_pct": 66.7}
    ]
    assert (summary["branches_total"], summary["branches_hit"]) == (2, 0)
