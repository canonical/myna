"""Shared Cobertura helpers for the coverage/dead-code tooling (feature 006).

One parser, one path normalization rule, consumed by both the populations
report (dev/coverage_populations.py) and the patch-cov normalization step
(research.md D3 wrinkle): every path is rewritten to repo-root-relative
(`client/...` or `server/...`) so git-diff paths and coverage paths align.

Cobertura model: <class filename="..."> with <line number hits branch ...>.
We treat a line as covered iff hits > 0.
"""

from __future__ import annotations

import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Path prefixes produced by each toolchain, and the repo-relative prefix they
# map to. cargo-llvm-cov emits workspace-relative paths (client/ is the
# workspace root); coverage.py emits paths relative to server/.
_PREFIX_MAP = {
    "client/": "",
    "server/": "",
    "src/myna/": "server/src/myna/",
    "myna-": "client/myna-",
}


@dataclass(frozen=True)
class LineHits:
    """Per-file line hit data from one Cobertura export."""

    # filename (repo-root-relative) -> {lineno: hits}
    files: dict[str, dict[int, int]]


def normalize_path(path: str) -> str:
    """Map a toolchain-relative coverage path to repo-root-relative."""
    # Absolute path under the repo (some coverage.py invocations emit these).
    if path.startswith("/"):
        try:
            return str(Path(path).resolve().relative_to(REPO_ROOT))
        except ValueError:
            return path
    for prefix, replacement in _PREFIX_MAP.items():
        if path.startswith(prefix):
            return replacement + path[len(prefix) :]
    # Bare crate/file paths from llvm-cov already look like client/... only if
    # the workspace root was the cwd; otherwise they arrive as e.g.
    # "myna-core/src/lib.rs" (handled by the "myna-" rule) or relative "../".
    if path.startswith("../server/"):
        return path[len("../") :]
    return path


def parse_cobertura(xml_path: Path) -> LineHits:
    """Parse a Cobertura XML export into per-line hit counts."""
    if not xml_path.exists():
        raise FileNotFoundError(f"coverage export missing: {xml_path}")
    root = ET.parse(xml_path).getroot()
    files: dict[str, dict[int, int]] = {}
    for cls in root.iter("class"):
        filename = cls.get("filename")
        if not filename:
            continue
        rel = normalize_path(filename)
        lines = files.setdefault(rel, {})
        for line in cls.iter("line"):
            number = line.get("number")
            if number is None:
                continue
            hits = int(line.get("hits", "0"))
            lineno = int(number)
            # Multiple <class> entries can name the same file (llvm-cov emits
            # one per crate); merge by taking the max hits per line.
            lines[lineno] = max(lines.get(lineno, 0), hits)
    return LineHits(files=files)


def covered_lines(hits: LineHits) -> set[tuple[str, int]]:
    """The (file, line) set with at least one recorded hit."""
    return {(f, n) for f, lines in hits.files.items() for n, h in lines.items() if h > 0}


def all_lines(hits: LineHits) -> set[tuple[str, int]]:
    """The (file, line) set present in the export at all (coverable lines)."""
    return {(f, n) for f, lines in hits.files.items() for n in lines}
