"""Every model fetcher pins the upstream revision it stages.

Unpinned, ``hf download <repo>`` and ``snapshot_download(<repo>)`` resolve that
repo's current ``main`` at fetch time. Two machines, or one machine two months
apart, then pack different weights into components labelled with the same snap
version - and nothing says so, because the "already present" guards skip on a
file existing rather than on identity. That is invisible to every other suite:
the adapters pass their unit tests whatever weights they load, and the
benchmark records a number without recording what produced it.

So assert it where it is cheap: a fetcher is text, and the pin is either in it
or it is not. Two revisions are duplicated across a bash/python pair (the same
upstream artifact is staged both for the packed component and for non-snap
adapter runs); the cross-checks below hold each pair together so a half-moved
pin cannot land.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

REPO_ROOT = Path(__file__).resolve().parents[2]

# Fetcher -> the pattern proving it names a revision. Kept explicit rather than
# globbed: a new fetcher should be a deliberate addition here, not something
# that silently opts out of pinning.
# Scripts staging several sizes hold a `declare -A REVISIONS=(...)` map. These
# are checked entry by entry rather than with a search: one pinned entry
# alongside a floating one would satisfy any "is there a SHA in here" test
# while still packing an unreproducible weight.
REVISION_MAPS = ("whisper-snap/dev/download-models.sh",)

PINNED = {
    # Single-model scripts.
    "parakeet-snap/dev/download-models.sh": r'^rev="murmure-model [0-9.]+"$',
    # Python fetchers.
    "dev/fetch_funasr_model.py": r'^REVISION = "v[0-9.]+"$',
    "dev/parakeet/fetch_parakeet_onnx.py": r'^RELEASE = "[0-9.]+"$',
    # NVIDIA's checkpoint the float exports are made from: a commit, and the
    # bytes, since the .nemo is re-exported rather than shipped.
    "dev/parakeet/export_parakeet_onnx.py": r'^REVISION = "[0-9a-f]{40}"$',
}


def _text(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


@pytest.mark.parametrize("rel", REVISION_MAPS, ids=REVISION_MAPS)
def test_every_revision_map_entry_is_a_commit(rel: str) -> None:
    body = re.search(r"declare -A REVISIONS=\((.*?)\n\)", _text(rel), re.S)
    assert body, f"{rel}: no `declare -A REVISIONS=(...)` map"
    entries = re.findall(r"^\s*\[([\w.]+)\]=(\S+)$", body.group(1), re.M)
    assert entries, f"{rel}: REVISIONS map is empty"
    floating = [k for k, v in entries if not re.fullmatch(r"[0-9a-f]{40}", v)]
    assert not floating, (
        f"{rel}: {floating} are not 40-char commits - a branch or tag can move "
        "under the pin, which is the whole thing being prevented"
    )


@pytest.mark.parametrize("rel", sorted(PINNED), ids=sorted(PINNED))
def test_fetcher_pins_a_revision(rel: str) -> None:
    assert re.search(PINNED[rel], _text(rel), re.M), (
        f"{rel} declares no pinned revision matching {PINNED[rel]!r} - an "
        "unpinned fetcher resolves the repo's current main, so the weights a "
        "build packs stop being reproducible"
    )


HF_DOWNLOADERS = REVISION_MAPS


@pytest.mark.parametrize("rel", HF_DOWNLOADERS, ids=HF_DOWNLOADERS)
def test_hf_download_passes_the_revision(rel: str) -> None:
    """A pin declared but not passed to `hf download` is decoration."""
    calls = [ln for ln in _text(rel).splitlines() if ln.lstrip().startswith("hf download")]
    assert calls, f"{rel}: no `hf download` invocation found"
    for line in calls:
        assert "--revision" in line, f"{rel}: `{line.strip()}` ignores the pin"


def test_parakeet_pins_agree() -> None:
    """The component stamp names the release the python fetcher downloads."""
    bash = re.search(
        r'^rev="murmure-model ([0-9.]+)"$', _text("parakeet-snap/dev/download-models.sh"), re.M
    )
    py = re.search(r'^RELEASE = "([0-9.]+)"$', _text("dev/parakeet/fetch_parakeet_onnx.py"), re.M)
    assert bash and py
    assert bash.group(1) == py.group(1), (
        "parakeet-snap/dev/download-models.sh stamps a different murmure-model "
        "release than dev/parakeet/fetch_parakeet_onnx.py downloads - the stamp would "
        "certify weights that were never staged"
    )


def test_parakeet_export_pins_agree() -> None:
    """The float components' stamp names the checkpoint and recipe the export
    tool writes, and the tool verifies the checkpoint's bytes."""
    tool = _text("dev/parakeet/export_parakeet_onnx.py")
    assert re.search(r'^SHA256 = "[0-9a-f]{64}"$', tool, re.M), (
        "dev/parakeet/export_parakeet_onnx.py downloads the checkpoint without a sha256"
    )
    repo = re.search(r'^REPO = "([^"]+)"$', tool, re.M)
    revision = re.search(r'^REVISION = "([0-9a-f]{40})"$', tool, re.M)
    recipe = re.search(r"^RECIPE = ([0-9]+)$", tool, re.M)
    stamp = re.search(r'^export_rev="(.+)"$', _text("parakeet-snap/dev/download-models.sh"), re.M)
    assert repo and revision and recipe and stamp
    assert stamp.group(1) == f"{repo.group(1)}@{revision.group(1)} recipe {recipe.group(1)}", (
        "parakeet-snap/dev/download-models.sh expects a different export than "
        "dev/parakeet/export_parakeet_onnx.py writes - it would re-export forever, or "
        "stage graphs made from another checkpoint"
    )


def test_parakeet_int8_component_stages_exactly_what_the_fetcher_writes() -> None:
    """The staging script names the files it copies instead of globbing the
    model cache, so the two lists have to agree: a file the fetcher adds would
    never reach the component, and one it drops would fail the copy."""
    staged = re.search(
        r'^model_files="([^"]+)"$', _text("parakeet-snap/dev/download-models.sh"), re.M
    )
    assert staged, "parakeet-snap/dev/download-models.sh names no model_files"
    fetched = re.search(
        r"^MODEL_FILES = \(\n((?:\s+\"[^\"]+\",\n)+)\)$",
        _text("dev/parakeet/fetch_parakeet_onnx.py"),
        re.M,
    )
    assert fetched, "dev/parakeet/fetch_parakeet_onnx.py names no MODEL_FILES"
    assert sorted(staged.group(1).split()) == sorted(re.findall(r'"([^"]+)"', fetched.group(1))), (
        "parakeet-snap/dev/download-models.sh stages a different file set than "
        "dev/parakeet/fetch_parakeet_onnx.py fetches"
    )
