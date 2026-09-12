#!/usr/bin/env python3
"""Run shexli JSON output through Myna's documented extension exemptions."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


def relative_paths(finding: dict[str, object], root: Path) -> set[str]:
    paths: set[str] = set()
    for evidence in finding.get("evidence", []):
        path = Path(evidence["path"])
        try:
            paths.add(path.relative_to(root).as_posix())
        except ValueError:
            paths.add(path.as_posix())
    return paths


def accepted(finding: dict[str, object], root: Path) -> str | None:
    """Return the documented exemption reason, or None for an actionable item."""
    rule = finding.get("rule_id")
    paths = relative_paths(finding, root)

    if (
        rule == "EGO-P-007"
        and paths
        and all(path.startswith("test/") and path.endswith(".test.js") for path in paths)
    ):
        return "standalone GJS contract tests run by test/run-suite.sh"
    if rule == "EGO-M-004" and paths == {"metadata.json"}:
        return "intentional GNOME Shell 46-51 support range"
    return None


def write_summary(lines: list[str]) -> None:
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with Path(summary).open("a", encoding="utf-8") as stream:
            stream.write("\n".join(lines) + "\n")
    else:
        print("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="extension root to scan")
    parser.add_argument("--shexli", default="shexli", help="shexli executable")
    args = parser.parse_args()
    root = args.root.resolve()

    result = subprocess.run(
        [args.shexli, str(root), "--format", "json"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode not in (0, 1):
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        print(f"shexli failed to run (exit {result.returncode})", file=sys.stderr)
        return result.returncode

    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        print(f"shexli did not produce valid JSON: {error}", file=sys.stderr)
        return 2

    known: list[tuple[dict[str, object], str]] = []
    actionable: list[dict[str, object]] = []
    for finding in report.get("findings", []):
        reason = accepted(finding, root)
        if reason:
            known.append((finding, reason))
        else:
            actionable.append(finding)

    lines = [
        "## shexli extension review",
        "",
        f"Scanned `{root}`: {len(known)} accepted finding(s), "
        f"{len(actionable)} actionable finding(s).",
        "",
        "### Accepted findings",
        "",
    ]
    for finding, reason in known:
        paths = ", ".join(f"`{path}`" for path in sorted(relative_paths(finding, root)))
        lines.append(f"- `{finding['rule_id']}` ({finding['severity']}): {reason} ({paths})")
    if not known:
        lines.append("- None")

    lines.extend(["", "### Actionable findings", ""])
    for finding in actionable:
        paths = ", ".join(f"`{path}`" for path in sorted(relative_paths(finding, root)))
        lines.append(
            f"- `{finding['rule_id']}` ({finding['severity']}): {finding['message']} ({paths})"
        )
    if not actionable:
        lines.append("- None")

    write_summary(lines)
    if actionable:
        return 1
    if result.returncode and not known:
        return result.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
