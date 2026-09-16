"""``dev/snap-version.sh``: the version a snap built from a checkout adopts.

Snap build instances mount only the snap directory, so snapcraft's
``version: git`` has no repository to describe. The script resolves the same
string on the host: ``0+git.<sha>`` with no annotated tag behind HEAD,
``<tag>+git<n>.<sha>`` past one, the bare tag on it, and ``-dirty`` when the
paths the snap packs differ from HEAD.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.repo_tree

SCRIPT = Path(__file__).resolve().parents[2] / "dev" / "snap-version.sh"

GIT_ENV = {
    **os.environ,
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.com",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.com",
}


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], env=GIT_ENV, check=True, capture_output=True, text=True
    ).stdout.strip()


def _commit(repo: Path, path: str, content: str) -> str:
    (repo / path).parent.mkdir(parents=True, exist_ok=True)
    (repo / path).write_text(content, encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", path)
    return _git(repo, "rev-parse", "--short", "HEAD")


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    (tmp_path / "dev").mkdir()
    shutil.copy2(SCRIPT, tmp_path / "dev" / "snap-version.sh")
    _git(tmp_path, "init", "-q")
    return tmp_path


def _version(repo: Path, *paths: str) -> str:
    return subprocess.run(
        [str(repo / "dev" / "snap-version.sh"), *paths],
        cwd="/",
        env=GIT_ENV,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def test_untagged_history_is_zero_plus_git_sha(repo: Path) -> None:
    sha = _commit(repo, "client/a", "1")
    assert _version(repo, "client") == f"0+git.{sha}"


def test_lightweight_tags_are_ignored(repo: Path) -> None:
    sha = _commit(repo, "client/a", "1")
    _git(repo, "tag", "backup")
    assert _version(repo, "client") == f"0+git.{sha}"


def test_on_an_annotated_tag_is_the_tag(repo: Path) -> None:
    _commit(repo, "client/a", "1")
    _git(repo, "tag", "-a", "v0.1.0", "-m", "v0.1.0")
    assert _version(repo, "client") == "v0.1.0"


def test_past_an_annotated_tag_counts_commits(repo: Path) -> None:
    _commit(repo, "client/a", "1")
    _git(repo, "tag", "-a", "v0.1.0", "-m", "v0.1.0")
    _commit(repo, "client/a", "2")
    sha = _commit(repo, "client/a", "3")
    assert _version(repo, "client") == f"v0.1.0+git2.{sha}"


def test_dirty_only_for_the_packed_paths(repo: Path) -> None:
    _commit(repo, "client/a", "1")
    sha = _commit(repo, "server/b", "1")
    (repo / "server" / "b").write_text("2", encoding="utf-8")
    assert _version(repo, "client") == f"0+git.{sha}"
    (repo / "client" / "a").write_text("2", encoding="utf-8")
    assert _version(repo, "client") == f"0+git.{sha}-dirty"
