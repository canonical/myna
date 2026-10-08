"""``myna-config-deb/snapshot-changelog.py``: the entry a snapshot deb carries.

A build past the last tag gets its own changelog entry, versioned and
targeted like the upload, listing the user-facing commits since the tag that
touch what the deb packs. The committed changelog is never edited.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.repo_tree

SCRIPT = Path(__file__).resolve().parents[2] / "myna-config-deb" / "snapshot-changelog.py"

GIT_ENV = {
    **os.environ,
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.com",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.com",
    "GIT_COMMITTER_DATE": "2026-10-08T12:00:00+01:00",
}

RELEASE = """\
myna-config (0.2.0-0ubuntu1) stonking; urgency=medium

  * Guided first-run setup.

 -- Charles <charles@example.com>  Tue, 07 Oct 2026 10:00:00 +0000
"""


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], env=GIT_ENV, check=True, capture_output=True, text=True
    ).stdout.strip()


def _commit(repo: Path, path: str, subject: str) -> None:
    file = repo / path
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_text(subject, encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", subject)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    _git(tmp_path, "init", "-q")
    (tmp_path / "myna-config-deb" / "debian").mkdir(parents=True)
    (tmp_path / "myna-config-deb" / "debian" / "changelog").write_text(RELEASE, encoding="utf-8")
    _commit(tmp_path, "client/myna-config/src/a.rs", "feat(config): before the tag")
    _git(tmp_path, "tag", "-a", "v0.2.0", "-m", "v0.2.0")
    return tmp_path


def _changelog(repo: Path, version: str = "0.2.0+git2.abc-0ubuntu1~ppa1") -> str:
    return subprocess.run(
        ["python3", str(SCRIPT), str(repo), version, "resolute"],
        env=GIT_ENV,
        check=True,
        capture_output=True,
        text=True,
        input=RELEASE,
    ).stdout


def _entry(changelog: str) -> str:
    return changelog.split("\n -- ")[0]


def test_a_snapshot_heads_the_changelog_and_keeps_the_release(repo: Path) -> None:
    _commit(repo, "client/myna-config/src/b.rs", "fix(config): turn the extension on")
    changelog = _changelog(repo)
    assert changelog.startswith(
        "myna-config (0.2.0+git2.abc-0ubuntu1~ppa1) resolute; urgency=medium\n"
    )
    assert changelog.endswith(RELEASE)
    trailer = changelog.split("\n\n")[2]
    # The release's maintainer, dated by the commit: the staged tree stays reproducible.
    assert trailer == " -- Charles <charles@example.com>  Thu, 08 Oct 2026 11:00:00 +0000"


def test_only_user_facing_changes_to_what_the_deb_packs_are_listed(repo: Path) -> None:
    _commit(repo, "client/myna-config/src/b.rs", "fix(config): turn the extension on")
    _commit(repo, "extensions/myna-shell/x.js", "feat(shell): show the level")
    _commit(repo, "client/myna-config/src/c.rs", "refactor(config): rename a thing")
    _commit(repo, "client/myna-config/src/d.rs", "fix(dev): a developer tool")
    _commit(repo, "myna-config-deb/build-source.sh", "feat(deb): packaging plumbing")
    _commit(repo, "server/myna/x.py", "fix(server): not in the deb")
    _commit(repo, "myna-config-deb/rules", "docs: not a change to the app")
    assert _entry(_changelog(repo)).splitlines()[2:] == [
        "  * Turn the extension on.",
        "  * Show the level.",
    ]


def test_long_subjects_wrap_under_their_bullet(repo: Path) -> None:
    _commit(
        repo,
        "client/myna-config/src/b.rs",
        "fix(config): enable an extension installed since login at the next one so it runs",
    )
    lines = _entry(_changelog(repo)).splitlines()[2:]
    assert lines == [
        "  * Enable an extension installed since login at the next one so it",
        "    runs.",
    ]
    assert all(len(line) <= 80 for line in lines)


def test_a_hyphenated_word_is_never_split(repo: Path) -> None:
    _commit(
        repo,
        "client/myna-config/src/b.rs",
        "fix(config): the dictation indicator starts at the next login when installed mid-session",
    )
    assert _entry(_changelog(repo)).splitlines()[2:] == [
        "  * The dictation indicator starts at the next login when installed",
        "    mid-session.",
    ]


def test_a_snapshot_with_nothing_user_facing_says_so(repo: Path) -> None:
    _commit(repo, "server/myna/x.py", "fix(server): not in the deb")
    assert _entry(_changelog(repo)).splitlines()[2:] == [
        "  * Snapshot of main with no user-facing changes since 0.2.0.",
    ]


def test_the_result_parses_as_a_debian_changelog(repo: Path, tmp_path: Path) -> None:
    if shutil.which("dpkg-parsechangelog") is None:
        pytest.skip("dpkg-dev is not installed")
    _commit(repo, "client/myna-config/src/b.rs", "fix(config): turn the extension on")
    path = tmp_path / "changelog"
    path.write_text(_changelog(repo), encoding="utf-8")
    fields = subprocess.run(
        ["dpkg-parsechangelog", "-l", str(path), "-S", "Version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    assert fields == "0.2.0+git2.abc-0ubuntu1~ppa1"
