#!/usr/bin/env python3
"""Head a snapshot's changelog with an entry of its own.

    snapshot-changelog.py <repo> <version> <series> < changelog > changelog

A build past the last tag is not that release, so build-source.sh stages this
entry above the release's rather than relabelling it. It lists the feat and
fix commits since the tag that touch what the deb packs, leaving out the
scopes dev/release-notes.sh leaves out and deb's own packaging. It takes the
release's maintainer and the commit's date, so the staged tree stays
reproducible.
"""

from __future__ import annotations

import datetime
import email.utils
import re
import subprocess
import sys
import textwrap

# What build-source.sh packs, as .github/workflows/deb.yml filters it.
PACKED = [
    "myna-config-deb",
    "client/myna-config",
    "client/myna-core",
    "client/data",
    "client/build-support",
    "client/Cargo.toml",
    "client/Cargo.lock",
    "extensions/myna-shell",
]
INTERNAL = re.compile(r"^(benchmarker|bench|e2e|dev|spread|ci|workshop|deb)$")
SUBJECT = re.compile(r"^(feat|fix)(\(([^)]*)\))?!?: (.+)$")


def git(repo: str, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", repo, *args], check=True, capture_output=True, text=True
    ).stdout.strip()


def bullet(subject: str) -> str:
    text = subject[0].upper() + subject[1:]
    if not text.endswith("."):
        text += "."
    return textwrap.fill(
        text,
        width=72,
        initial_indent="  * ",
        subsequent_indent="    ",
        break_on_hyphens=False,
    )


def main(repo: str, version: str, series: str) -> None:
    changelog = sys.stdin.read()
    tag = git(repo, "describe", "--abbrev=0")
    subjects = git(
        repo, "log", "--reverse", "--no-merges", "--format=%s", f"{tag}..HEAD", "--", *PACKED
    ).splitlines()
    bullets = [
        bullet(match[4])
        for match in map(SUBJECT.match, subjects)
        if match and not INTERNAL.match(match[3] or "")
    ]
    if not bullets:
        bullets = [bullet(f"snapshot of main with no user-facing changes since {tag[1:]}")]
    maintainer = re.search(r"^ -- (.+?)  ", changelog, re.MULTILINE)
    if maintainer is None:
        sys.exit("snapshot-changelog: the changelog names no maintainer")
    committed = int(git(repo, "log", "-1", "--format=%ct"))
    date = email.utils.format_datetime(datetime.datetime.fromtimestamp(committed, datetime.UTC))
    sys.stdout.write(
        f"myna-config ({version}) {series}; urgency=medium\n\n"
        + "\n".join(bullets)
        + f"\n\n -- {maintainer[1]}  {date}\n\n"
        + changelog
    )


if __name__ == "__main__":
    main(*sys.argv[1:])
