"""dev/lcov_cobertura.py: the e2e suites' coverage reaches the patch gate.

What the gate reads back through coverage_lib must name the same
repo-relative lines as the suites' own exports, or e2e coverage silently
counts for nothing.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.repo_tree

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "dev"))

import coverage_lib  # noqa: E402
import lcov_cobertura  # noqa: E402
import patch_cov  # noqa: E402

LCOV = """\
SF:/project/client/myna-settings/src/app.rs
DA:10,3
DA:11,0
DA:10,5
end_of_record
SF:/home/workshop/.cargo/registry/src/glib/lib.rs
DA:1,9
end_of_record
SF:/project/client/myna-core/src/lib.rs
DA:4,0
end_of_record
"""


def export(lcov_dir: Path, out: Path, tree: str) -> Path:
    lcov = lcov_dir / "e2e.lcov"
    lcov.write_text(LCOV)
    argv = [str(lcov), str(out), "--root", "/project/client/", "--client-tree", tree]
    sys.argv = ["lcov_cobertura.py", *argv]
    assert lcov_cobertura.main() == 0
    return out


def test_gate_reads_e2e_lines_repo_relative(tmp_path: Path) -> None:
    out = export(tmp_path, tmp_path / "e2e.cobertura.xml", "abc")

    hits = coverage_lib.parse_cobertura(out)

    assert hits.files == {
        "client/myna-settings/src/app.rs": {10: 5, 11: 0},
        "client/myna-core/src/lib.rs": {4: 0},
    }
    assert coverage_lib.covered_lines(hits) == {("client/myna-settings/src/app.rs", 10)}


def test_gate_counts_only_exports_of_heads_client_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    cov = tmp_path / "client/target/coverage"
    cov.mkdir(parents=True)
    fresh = export(tmp_path, cov / "e2e-noble.cobertura.xml", "head")
    export(tmp_path, cov / "e2e-resolute.cobertura.xml", "older")
    monkeypatch.setattr(patch_cov, "REPO_ROOT", tmp_path)

    assert patch_cov.e2e_exports("head") == [fresh]
    assert (
        "ignoring e2e-resolute.cobertura.xml, built from another client/ (older)"
        in capsys.readouterr().out
    )
