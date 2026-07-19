#!/usr/bin/env python3
"""
Validate every ```mermaid block in the markdown files under docs/ and specs/.

Usage:
    dev/check-mermaid.py [--puppeteer-cfg PATH] [FILE ...]

If no FILEs are given it scans docs/ and specs/ (relative to the repo root,
which is assumed to be the cwd or the directory two levels above this script).

Exit code: 0 if all blocks are valid, 1 if any block fails.

Requires:
    @mermaid-js/mermaid-cli  (npx -y @mermaid-js/mermaid-cli)
    A Chromium/Chrome browser pointed to by PUPPETEER_EXECUTABLE_PATH or
    --puppeteer-cfg (a JSON file with {"executablePath": "...", "args": [...]}).
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path


def find_md_files(roots: list[Path]) -> list[Path]:
    files = []
    for root in roots:
        if root.is_file():
            files.append(root)
        else:
            files.extend(sorted(root.rglob("*.md")))
    return files


def extract_blocks(text: str) -> list[str]:
    return re.findall(r"```mermaid\n(.*?)```", text, re.DOTALL)


def validate_block(block: str, puppeteer_cfg: str | None, block_label: str) -> bool:
    with tempfile.NamedTemporaryFile(
        suffix=".mmd", mode="w", delete=False
    ) as f_in, tempfile.NamedTemporaryFile(
        suffix=".svg", mode="w", delete=False
    ) as f_out:
        f_in.write(block)
        f_in_path = f_in.name
        f_out_path = f_out.name

    try:
        cmd = ["npx", "-y", "@mermaid-js/mermaid-cli"]
        if puppeteer_cfg:
            cmd += ["-p", puppeteer_cfg]
        cmd += ["-i", f_in_path, "-o", f_out_path]

        result = subprocess.run(
            cmd, capture_output=True, text=True, timeout=60
        )

        ok = Path(f_out_path).exists() and "<svg" in Path(f_out_path).read_text()
        if not ok:
            print(f"  {block_label}: FAIL")
            for line in result.stderr.splitlines():
                if re.search(r"error|parse|syntax", line, re.I):
                    print(f"    {line}")
        else:
            print(f"  {block_label}: OK")
        return ok

    finally:
        Path(f_in_path).unlink(missing_ok=True)
        Path(f_out_path).unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--puppeteer-cfg",
        metavar="PATH",
        help="Path to a Puppeteer JSON config (e.g. to pass --no-sandbox args)",
    )
    parser.add_argument(
        "files",
        nargs="*",
        metavar="FILE",
        help="Markdown files to check (default: docs/ and specs/)",
    )
    args = parser.parse_args()

    # Resolve repo root: two dirs up from this script (dev/check-mermaid.py).
    repo_root = Path(__file__).resolve().parent.parent

    if args.files:
        md_files = find_md_files([Path(f) for f in args.files])
    else:
        md_files = find_md_files([repo_root / "docs", repo_root / "specs"])

    puppeteer_cfg = args.puppeteer_cfg

    # If PUPPETEER_EXECUTABLE_PATH is set and no explicit config, synthesise one.
    if not puppeteer_cfg and os.environ.get("PUPPETEER_EXECUTABLE_PATH"):
        tmp = tempfile.NamedTemporaryFile(
            suffix=".json", mode="w", delete=False
        )
        json.dump(
            {
                "executablePath": os.environ["PUPPETEER_EXECUTABLE_PATH"],
                "args": ["--no-sandbox", "--disable-setuid-sandbox"],
            },
            tmp,
        )
        tmp.close()
        puppeteer_cfg = tmp.name

    total = 0
    failed = 0

    for md in md_files:
        text = md.read_text()
        blocks = extract_blocks(text)
        if not blocks:
            continue
        rel = md.relative_to(repo_root) if md.is_relative_to(repo_root) else md
        print(f"=== {rel} ({len(blocks)} block(s)) ===")
        for i, block in enumerate(blocks, 1):
            total += 1
            if not validate_block(block, puppeteer_cfg, f"#{i}"):
                failed += 1

    if total == 0:
        print("No Mermaid blocks found.")
        return 0

    print(f"\n{total - failed}/{total} block(s) OK", end="")
    if failed:
        print(f", {failed} FAILED")
        return 1
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
