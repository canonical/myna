"""Packaging invariants of the text-provider snaps, myna-text and myna-translation.

The speech snaps are held together by test_snap_packaging.py; these snaps have
no modelctl and no engines, so they get their own few promises: they are
offline, each shares itself as a text provider, its version is the tree's, the
components it declares are the ones its model manifests describe, and neither
carries the other's runtime.
"""

from __future__ import annotations

import json
import shlex
import tomllib
from pathlib import Path

import pytest
import yaml

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

ROOT = Path(__file__).resolve().parents[2]
TEXT = ROOT / "text-snap"
TRANSLATION = ROOT / "translation-snap"
SNAPS = {"myna-text": TEXT, "myna-translation": TRANSLATION}
COMPONENT_IDS = {
    "myna-text": {"myna-disfluency-en": "text.cleanup"},
    # Every translation pair, one plugin file each: the manifests follow them.
    "myna-translation": {
        tomllib.loads(path.read_text(encoding="utf-8"))["id"]: "text.translate"
        for path in sorted((TRANSLATION / "plugins").glob("*.toml"))
    },
}


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def recipe_of(snap_dir: Path) -> dict:
    return yaml.safe_load(read(snap_dir / "snap/snapcraft.yaml"))


@pytest.fixture(scope="module")
def recipe() -> dict:
    return recipe_of(TEXT)


@pytest.fixture(scope="module")
def translation() -> dict:
    return recipe_of(TRANSLATION)


@pytest.fixture(params=sorted(SNAPS))
def snap(request) -> tuple[str, Path, dict]:
    return request.param, SNAPS[request.param], recipe_of(SNAPS[request.param])


def test_each_snap_is_strictly_confined_and_offline(snap):
    name, _, recipe = snap
    assert recipe["name"] == name
    assert recipe["confinement"] == "strict"
    plugs = [p for app in recipe["apps"].values() for p in app.get("plugs", [])]
    assert "network" not in plugs
    assert "network-bind" in plugs
    assert "network" not in recipe.get("plugs", {})


def test_onnxruntime_reads_pci_so_only_the_tagger_snap_plugs_hardware_observe(recipe, translation):
    plugs = [p for app in recipe["apps"].values() for p in app.get("plugs", [])]
    assert "hardware-observe" in plugs
    plugs = [p for app in translation["apps"].values() for p in app.get("plugs", [])]
    assert "hardware-observe" not in plugs


def test_each_snap_takes_its_version_from_the_tree(snap):
    _, snap_dir, recipe = snap
    assert recipe["adopt-info"] == "version"
    assert "version" not in recipe
    assert recipe["parts"]["version"]["parse-info"] == ["version.metainfo.xml"]
    assert "0.0.0" not in read(snap_dir / "dev/prepare.sh").replace(
        "myna-0.0.0-py3-none-any.whl", ""
    )


def test_each_daemon_serves_the_inference_provider_share_as_a_text_role(snap):
    name, _, recipe = snap
    slot = recipe["slots"]["provider"]
    assert (slot["interface"], slot["content"]) == ("content", "inference-provider")
    share = slot["source"]["write"][0]
    assert share == "$SNAP_COMMON/share/provider"
    app = recipe["apps"]["server"]
    assert app["daemon"] == "simple"
    argv = shlex.split(app["command"])
    assert argv[0] == f"bin/{name}"
    assert argv[argv.index("--socket") + 1].startswith(share + "/")
    assert "--share-provider" in argv


def test_every_component_has_a_model_manifest_and_every_manifest_a_component(snap):
    name, snap_dir, recipe = snap
    organized = recipe["parts"]["model-components"]["organize"]
    staged = {src.split("/")[0]: dst for src, dst in organized.items()}
    assert set(staged) == set(recipe["components"])
    for component, dst in staged.items():
        assert dst == f"(component/{component})/"

    manifests = {}
    for path in (snap_dir / "models").glob("*/myna-text-model.json"):
        manifests[path.parent.name] = json.loads(read(path))
    assert {m["id"]: m["task"] for m in manifests.values()} == COMPONENT_IDS[name]
    for component in recipe["components"]:
        assert (snap_dir / "plugins" / f"{component.removeprefix('model-')}.toml").is_file()


def test_the_runtimes_are_not_shared(recipe, translation):
    # No cross-contamination: the tagger snap has no bergamot, the translation
    # snap no onnxruntime, in code, licences, notices or packages.
    assert "bergamot" not in recipe["parts"]
    assert "bergamot" not in read(TEXT / "snap/snapcraft.yaml").replace("Translation lives", "")
    assert not (TEXT / "bergamot").exists()
    assert not (TEXT / "dev/build-bergamot.sh").exists()
    assert "bergamot" not in read(TEXT / "NOTICE").lower()
    assert "MPL" not in read(TEXT / "NOTICE")
    assert "--extra text" in read(TEXT / "dev/prepare.sh")

    assert "bergamot" in translation["parts"]
    assert "onnxruntime" not in read(TRANSLATION / "NOTICE").replace("no ONNX Runtime", "")
    assert "--extra translation" in read(TRANSLATION / "dev/prepare.sh")
    assert "--extra text" not in read(TRANSLATION / "dev/prepare.sh")
    gate = translation["parts"]["server-app"]["override-build"]
    assert "find_spec('onnxruntime') is None" in gate
    assert "myna.translation.cli" in gate
    assert "onnxruntime" in recipe["parts"]["server-app"]["override-build"]
    assert "myna.text.cli" in recipe["parts"]["server-app"]["override-build"]


def test_the_bergamot_runtime_is_built_from_a_pinned_commit_without_mkl(translation):
    part = translation["parts"]["bergamot"]
    assert len(part["source-commit"]) == 40
    assert part["source"].startswith("https://github.com/browsermt/")
    build = read(TRANSLATION / "dev/build-bergamot.sh")
    assert "-DUSE_MKL=OFF" in build
    assert "-DCOMPILE_PYTHON=OFF" in build
    assert "usr/share/doc/myna-translation/bergamot" in build
    assert part["source-commit"] in read(TRANSLATION / "NOTICE")


def test_the_mozilla_attribution_travels_with_the_translation_snap(translation):
    notice = read(TRANSLATION / "NOTICE")
    assert "Mozilla Public License 2.0" in notice
    assert "MPL-2.0" in notice
    component = read(TRANSLATION / "models/translate-en-fr/NOTICE")
    for sha in (
        "6322e296d4fecfe395a8d5723da4ec37ecbe6d7613bb1dfcf4b28e2a47498b68",
        "2585ed98d3af0bc949865aedeb390493d591f56870814376e73e4144c41ed059",
        "783abf3abe075afdf8d85d233994bef2c3a064e935ab1bed946820aff6ac002a",
    ):
        assert sha in component
        assert sha in read(TRANSLATION / "plugins/translate-en-fr.toml")
    assert 'license = "MPL-2.0"' in read(TRANSLATION / "plugins/translate-en-fr.toml")
    assert "MPL" not in read(TEXT / "models/disfluency-en/NOTICE")


@pytest.mark.parametrize("pair", ["en-fr", "en-de"])
def test_the_translate_config_names_files_the_component_ships(pair):
    config = yaml.safe_load(read(TRANSLATION / f"models/translate-{pair}/config.yml"))
    script = read(TRANSLATION / f"plugins/translate-{pair}.toml")
    files = [*config["models"], *config["vocabs"], config["shortlist"][0]]
    assert files
    for name in files:
        assert name in script
    # Beam 1 is what the quality and latency in the manifest were measured at.
    assert config["beam-size"] == 1


def test_the_two_snaps_are_built_by_make_and_published_by_neither():
    makefile = read(ROOT / "Makefile")
    assert "SNAPS := $(BACKENDS) text translation myna" in makefile
    assert "PUBLISHED := $(filter-out fake text translation,$(SNAPS))" in makefile


def test_every_shipped_manifest_declares_contract_2_and_lists_as_the_client_reads_it(snap):
    from myna.text.protocol import declared

    name, snap_dir, _ = snap
    for path in (snap_dir / "models").glob("*/myna-text-model.json"):
        meta = json.loads(read(path))
        assert meta["contract"] == 2
        fields = declared(meta)
        assert {
            "class",
            "needs",
            "params",
            "license",
            "attribution",
            "download_bytes",
            "source",
        } <= set(fields)
        # in_lang is covers.language and out_lang covers.to: no second copy.
        assert "in_lang" not in meta
        assert "out_lang" not in meta
        named = {p["name"] for p in fields["params"]}
        assert {"language"} | ({"to"} if meta["task"] == "text.translate" else set()) == named
