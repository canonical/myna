"""Packaging invariants of the myna-text snap.

The speech snaps are held together by test_snap_packaging.py; this snap has no
modelctl and no engines, so it gets its own few promises: it is offline, it
shares itself as a text provider, its version is the tree's, and the components
it declares are the ones its model manifests describe.
"""

from __future__ import annotations

import json
import shlex
from pathlib import Path

import pytest
import yaml

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

SNAP_DIR = Path(__file__).resolve().parents[2] / "text-snap"


@pytest.fixture(scope="module")
def recipe() -> dict:
    return yaml.safe_load((SNAP_DIR / "snap/snapcraft.yaml").read_text(encoding="utf-8"))


def test_the_snap_is_strictly_confined_and_offline(recipe):
    assert recipe["name"] == "myna-text"
    assert recipe["confinement"] == "strict"
    plugs = [p for app in recipe["apps"].values() for p in app.get("plugs", [])]
    assert "network" not in plugs
    assert "network-bind" in plugs
    # onnxruntime's device discovery reads /sys/bus/pci/devices.
    assert "hardware-observe" in plugs
    assert "network" not in recipe.get("plugs", {})


def test_the_snap_takes_its_version_from_the_tree(recipe):
    assert recipe["adopt-info"] == "version"
    assert "version" not in recipe
    assert recipe["parts"]["version"]["parse-info"] == ["version.metainfo.xml"]
    assert "0.0.0" not in (SNAP_DIR / "dev/prepare.sh").read_text(encoding="utf-8").replace(
        "myna-0.0.0-py3-none-any.whl", ""
    )


def test_the_daemon_serves_the_inference_provider_share_as_a_text_role(recipe):
    slot = recipe["slots"]["provider"]
    assert (slot["interface"], slot["content"]) == ("content", "inference-provider")
    share = slot["source"]["write"][0]
    assert share == "$SNAP_COMMON/share/provider"
    app = recipe["apps"]["server"]
    assert app["daemon"] == "simple"
    argv = shlex.split(app["command"])
    assert argv[0] == "bin/myna-text"
    assert argv[argv.index("--socket") + 1].startswith(share + "/")
    assert "--share-provider" in argv


def test_every_component_has_a_model_manifest_and_every_manifest_a_component(recipe):
    organized = recipe["parts"]["model-components"]["organize"]
    staged = {src.split("/")[0]: dst for src, dst in organized.items()}
    assert set(staged) == set(recipe["components"])
    for name, dst in staged.items():
        assert dst == f"(component/{name})/"

    manifests = {}
    for path in (SNAP_DIR / "models").glob("*/myna-text-model.json"):
        manifests[path.parent.name] = json.loads(path.read_text(encoding="utf-8"))
    assert {m["id"] for m in manifests.values()} == {
        "myna-disfluency-en",
        "bergamot-base-memory-en-fr",
    }
    assert {m["task"] for m in manifests.values()} == {"text.cleanup", "text.translate"}
    for name in recipe["components"]:
        assert name in (SNAP_DIR / "dev/download-models.sh").read_text(encoding="utf-8")


def test_the_bergamot_runtime_is_built_from_a_pinned_commit_without_mkl(recipe):
    part = recipe["parts"]["bergamot"]
    assert len(part["source-commit"]) == 40
    assert part["source"].startswith("https://github.com/browsermt/")
    build = (SNAP_DIR / "dev/build-bergamot.sh").read_text(encoding="utf-8")
    assert "-DUSE_MKL=OFF" in build
    assert "-DCOMPILE_PYTHON=OFF" in build
    notice = (SNAP_DIR / "NOTICE").read_text(encoding="utf-8")
    assert part["source-commit"] in notice


def test_the_translate_config_names_files_the_component_ships():
    config = yaml.safe_load((SNAP_DIR / "models/translate-en-fr/config.yml").read_text())
    script = (SNAP_DIR / "dev/download-models.sh").read_text(encoding="utf-8")
    files = [*config["models"], *config["vocabs"], config["shortlist"][0]]
    assert files
    for name in files:
        assert name in script
    # Beam 1 is what the quality and latency in the manifest were measured at.
    assert config["beam-size"] == 1
