"""Tests for the dead-code report generator (dev/coverage_populations.py).

The report is what the project reads its debt number off, so its arithmetic
and its staleness guards are worth pinning: a wrong number here is worse than
no number, because it is believed.
"""

from __future__ import annotations

import sys
from pathlib import Path
from xml.sax.saxutils import escape

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "dev"))

import coverage_populations as cp  # noqa: E402


def cobertura(tmp_path: Path, name: str, files: dict[str, dict[int, int]]) -> Path:
    """A minimal Cobertura export: {filename: {line: hits}}.

    Filenames are toolchain-relative, as the real exports carry them
    (llvm-cov: workspace-relative `myna-*/...`; coverage.py: `src/myna/...`);
    coverage_lib.normalize_path is what makes them repo-relative.
    """
    classes = []
    for filename, lines in files.items():
        rows = "".join(
            f'<line number="{n}" hits="{h}" branch="false"/>' for n, h in sorted(lines.items())
        )
        classes.append(f'<class filename="{filename}"><lines>{rows}</lines></class>')
    path = tmp_path / name
    path.write_text(
        f'<?xml version="1.0"?><coverage><packages><package><classes>'
        f"{''.join(classes)}</classes></package></packages></coverage>"
    )
    return path


@pytest.fixture
def sources(tmp_path, monkeypatch):
    """A stand-in repo root, so 'does this file still exist' is under test control."""
    monkeypatch.setattr(cp, "REPO_ROOT", tmp_path)
    return tmp_path


def touch(root: Path, rel: str, body: str = "") -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)


class TestComponent:
    def test_rust_rolls_up_to_the_crate(self):
        assert cp.component_of("client/myna-audio/src/native.rs") == "client/myna-audio"

    def test_python_rolls_up_to_the_subpackage(self):
        # One "server/myna" bucket would hide which half of the tree is dead.
        assert cp.component_of("server/src/myna/testbed/whisper.py") == "server/myna/testbed"
        assert cp.component_of("server/src/myna/core/events.py") == "server/myna/core"

    def test_module_directly_under_the_package_keeps_the_package_bucket(self):
        assert cp.component_of("server/src/myna/__init__.py") == "server/myna"


class TestPopulation:
    def test_lines_split_into_the_three_populations(self, sources, tmp_path):
        touch(sources, "client/myna-core/src/lib.rs")
        tests = cobertura(tmp_path, "t.xml", {"myna-core/src/lib.rs": {1: 3, 2: 0, 3: 0}})
        merged = cobertura(tmp_path, "m.xml", {"myna-core/src/lib.rs": {1: 3, 2: 7, 3: 0}})

        pop = cp.Population(cp.parse_cobertura(tests), cp.parse_cobertura(merged))

        assert pop.totals == {"test_covered": 1, "usecase_only": 1, "never_executed": 1}
        assert pop.dead == {"client/myna-core/src/lib.rs": [3]}

    def test_a_file_the_tree_no_longer_has_is_excluded_and_flagged(self, sources, tmp_path):
        touch(sources, "client/myna-core/src/live.rs")
        files = {"myna-core/src/live.rs": {1: 0}, "myna-core/src/gone.rs": {1: 0}}
        export = cobertura(tmp_path, "t.xml", files)

        pop = cp.Population(cp.parse_cobertura(export), cp.parse_cobertura(export))

        # Deleted code is not debt, and counting it inflates the headline.
        assert pop.stale == ["client/myna-core/src/gone.rs"]
        assert pop.totals["never_executed"] == 1

    def test_the_universe_is_the_union_of_both_exports(self, sources, tmp_path):
        touch(sources, "client/myna-core/src/lib.rs")
        tests = cobertura(tmp_path, "t.xml", {"myna-core/src/lib.rs": {1: 1}})
        merged = cobertura(tmp_path, "m.xml", {"myna-core/src/lib.rs": {1: 1, 2: 0}})

        pop = cp.Population(cp.parse_cobertura(tests), cp.parse_cobertura(merged))

        assert cp.coverable(pop.totals) == 2


class TestRanges:
    def test_contiguous_lines_collapse_to_spans(self):
        assert cp.fmt_ranges([1, 2, 3, 7, 9, 10]) == "1-3, 7, 9-10"

    def test_the_span_list_is_capped_and_says_so(self):
        nums = list(range(1, 40, 2))  # 20 single-line spans
        assert cp.fmt_ranges(nums, limit=3) == "1, 3, 5, … (+17 more spans)"


class TestRustFunctions:
    def test_monomorphizations_and_closures_fold_into_one_function(self, sources, tmp_path):
        touch(sources, "client/myna-core/src/lib.rs")
        base = "<myna_core::T>::run"
        entries = [
            (f"{base}::<u8>", 0),
            (f"{base}::<u16>", 4),
            (f"{base}::{{closure#0}}", 0),
            ("<myna_core::T>::cold", 0),
        ]
        methods = "".join(
            f'<method name="{escape(name, {chr(34): "&quot;"})}">'
            f'<lines><line number="10" hits="{hits}"/></lines></method>'
            for name, hits in entries
        )
        path = tmp_path / "merged.xml"
        path.write_text(
            f'<?xml version="1.0"?><coverage><packages><package><classes>'
            f'<class filename="myna-core/src/lib.rs"><methods>{methods}</methods>'
            f"<lines/></class></classes></package></packages></coverage>"
        )

        dead = cp.rust_dead_functions(path)

        # `run` was entered through one instantiation, so it is not dead; its
        # cold closure is a branch, not a function. Only `cold` is a finding.
        assert dead == {"client/myna-core/src/lib.rs": ["<myna_core::T>::cold"]}


class TestPythonFunctions:
    def _pop(self, sources, tmp_path, body: str, hits: dict[int, int]) -> cp.Population:
        touch(sources, "server/src/myna/core/m.py", body)
        export = cobertura(tmp_path, "e.xml", {"src/myna/core/m.py": hits})
        return cp.Population(cp.parse_cobertura(export), cp.parse_cobertura(export))

    def test_a_def_whose_body_never_ran_is_a_finding(self, sources, tmp_path):
        body = "def live():\n    return 1\n\n\ndef cold():\n    return 2\n"
        pop = self._pop(sources, tmp_path, body, {2: 5, 6: 0})

        assert cp.python_dead_functions(pop) == {"server/src/myna/core/m.py": ["cold (L6)"]}

    def test_a_def_nested_in_a_dead_def_is_not_a_second_finding(self, sources, tmp_path):
        body = "def outer():\n    def inner():\n        return 1\n\n    return inner\n"
        pop = self._pop(sources, tmp_path, body, {2: 0, 3: 0, 5: 0})

        assert cp.python_dead_functions(pop) == {"server/src/myna/core/m.py": ["outer (L2)"]}


class TestStaticVerdict:
    def test_a_tool_that_did_not_run_never_reads_as_clean(self):
        static = cp.Static("cargo machete", ["cargo", "machete"], "`x` not installed", "skipped")
        assert static.verdict == "not run (`x` not installed)"

    def test_findings_are_counted(self):
        static = cp.Static("vulture", ["vulture"], "a\n\nb\n", "findings")
        assert static.verdict == "2 line(s) of findings"
