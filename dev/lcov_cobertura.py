#!/usr/bin/env python3
"""Convert an llvm-cov lcov export into the Cobertura subset patch_cov reads.

The e2e suites run a myna-config built apart from `cargo llvm-cov`'s (on the
GTK floor, in another workshop), so their profiles cannot join that merge and
are exported on their own. diff-cover refuses to mix lcov with XML reports,
hence Cobertura, shaped like cargo-llvm-cov's: filenames relative to the
workspace root (--root), which is also the export's <source>. --client-tree
stamps the client/ tree the binaries were built from (`git rev-parse
HEAD:client`): patch_cov ignores an export of another tree, whose line
numbers may no longer match.

Usage: dev/lcov_cobertura.py IN.lcov OUT.xml --root /project/client --client-tree ID
"""

from __future__ import annotations

import argparse
import xml.etree.ElementTree as ET
from pathlib import Path


def parse_lcov(text: str, root: str) -> dict[str, dict[int, int]]:
    """Per-file line hits from lcov `SF:`/`DA:` records under root."""
    files: dict[str, dict[int, int]] = {}
    lines: dict[int, int] | None = None
    for raw in text.splitlines():
        if raw.startswith("SF:"):
            path = raw[3:]
            prefix = root.rstrip("/") + "/"
            lines = files.setdefault(path[len(prefix) :], {}) if path.startswith(prefix) else None
        elif raw.startswith("DA:") and lines is not None:
            number, hits = raw[3:].split(",")[:2]
            lineno = int(number)
            lines[lineno] = max(lines.get(lineno, 0), int(hits))
        elif raw == "end_of_record":
            lines = None
    return files


def to_cobertura(files: dict[str, dict[int, int]], root: str, client_tree: str) -> ET.ElementTree:
    top = ET.Element("coverage", version="1", client_tree=client_tree)
    ET.SubElement(ET.SubElement(top, "sources"), "source").text = root.rstrip("/")
    classes = ET.SubElement(
        ET.SubElement(ET.SubElement(top, "packages"), "package", name="e2e"), "classes"
    )
    for path in sorted(files):
        cls = ET.SubElement(classes, "class", name=path, filename=path)
        out = ET.SubElement(cls, "lines")
        for lineno in sorted(files[path]):
            ET.SubElement(out, "line", number=str(lineno), hits=str(files[path][lineno]))
    return ET.ElementTree(top)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("lcov", type=Path)
    ap.add_argument("out", type=Path)
    ap.add_argument("--root", required=True)
    ap.add_argument("--client-tree", required=True)
    args = ap.parse_args()
    files = parse_lcov(args.lcov.read_text(), args.root)
    to_cobertura(files, args.root, args.client_tree).write(
        args.out, encoding="unicode", xml_declaration=True
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
