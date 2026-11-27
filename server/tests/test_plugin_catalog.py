"""The plugin sources, and what dev/gen-plugins.py makes of them.

Adding a plugin is one plugins/<name>.toml; everything else is generated, so
the promises are: every source is valid for both snaps, the committed outputs
are what the generator writes now, and a plugin outside the licence allow-list
or with a malformed pin is refused.
"""

from __future__ import annotations

import copy
import importlib.util
import json
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

ROOT = Path(__file__).resolve().parents[2]
SNAP_DIRS = [ROOT / "text-snap", ROOT / "translation-snap"]
SCRIPT = ROOT / "dev/gen-plugins.py"

spec = importlib.util.spec_from_file_location("gen_plugins", SCRIPT)
assert spec and spec.loader
gen = importlib.util.module_from_spec(spec)
sys.modules["gen_plugins"] = gen
spec.loader.exec_module(gen)


def plugins(snap_dir: Path) -> list[dict]:
    return [gen.read_plugin(p) for p in sorted((snap_dir / "plugins").glob("*.toml"))]


ALL = [(d, p) for d in SNAP_DIRS for p in plugins(d)]


@pytest.fixture
def valid() -> dict:
    return copy.deepcopy(ALL[0][1])


@pytest.mark.parametrize("plugin", [p for _, p in ALL], ids=lambda p: p["component"])
def test_every_shipped_plugin_validates(plugin):
    gen.validate(plugin, plugin["component"])
    assert plugin["license"] in ("Apache-2.0", "MIT", "MPL-2.0", "CC-BY-4.0") or plugin[
        "license"
    ].startswith("BSD")


def test_component_names_are_unique_across_both_snaps():
    names = [p["component"] for _, p in ALL]
    assert len(names) == len(set(names))
    ids = [p["id"] for _, p in ALL]
    assert len(ids) == len(set(ids))


def test_committed_outputs_are_what_the_generator_writes():
    for snap_dir in SNAP_DIRS:
        for path, text in gen.outputs_of(snap_dir).items():
            assert path.read_text(encoding="utf-8") == text, path


def test_generation_is_idempotent_and_check_mode_passes():
    first = gen.outputs_of(SNAP_DIRS[0])
    assert gen.outputs_of(SNAP_DIRS[0]) == first
    done = subprocess.run(
        [sys.executable, str(SCRIPT), "--check", *map(str, SNAP_DIRS)],
        capture_output=True,
        text=True,
    )
    assert done.returncode == 0, done.stderr


def test_check_mode_fails_on_drift_and_a_run_repairs_it(tmp_path):
    copy_dir = tmp_path / "text-snap"
    shutil.copytree(SNAP_DIRS[0], copy_dir, ignore=shutil.ignore_patterns("components", "wheels"))
    (copy_dir / "catalog.json").write_text("{}\n")
    cmd = [sys.executable, str(SCRIPT)]
    done = subprocess.run([*cmd, "--check", str(copy_dir)], capture_output=True, text=True)
    assert done.returncode == 1
    assert "catalog.json" in done.stderr
    subprocess.run([*cmd, str(copy_dir)], check=True, capture_output=True)
    subprocess.run([*cmd, "--check", str(copy_dir)], check=True, capture_output=True)


@pytest.mark.parametrize("snap_dir", SNAP_DIRS, ids=lambda d: d.name)
def test_the_catalog_lists_every_plugin_with_the_ux_fields(snap_dir):
    catalog = json.loads((snap_dir / "catalog.json").read_text())
    assert catalog["catalog"] == 1
    entries = {e["id"]: e for e in catalog["plugins"]}
    assert set(entries) == {p["component"] for p in plugins(snap_dir)}
    for entry in entries.values():
        assert entry["snap"] == catalog["snap"]
        assert entry["component"] == entry["id"]
        step = entry["step"]
        assert step["stage"] in gen.STAGES
        assert step["label_key"] == f"step.{step['id']}.name"
        assert step["description_key"] == f"step.{step['id']}.description"
        assert step["example_key"] == f"step.{step['id']}.example"
        read = entry["covers"]["language"]
        assert entry["in_lang"] in (read if isinstance(read, list) else [read])
        assert entry.get("languages", read) == read
        assert entry["out_lang"] == entry["covers"].get("to", entry["in_lang"])
        assert entry["installed_bytes"] == sum(f["bytes"] for f in entry["files"])
        assert entry["download_bytes"] > 0
        assert 0 <= entry["quality"]["value"] <= 1 and entry["quality"]["source"]
        assert entry["latency_class"] in ("fast", "ok", "slow")
        assert entry["locales"] and isinstance(entry["curated"], bool)
        assert entry["license"] and entry["attribution"]


def test_the_catalog_and_the_manifest_agree_on_what_the_daemon_reports():
    for snap_dir in SNAP_DIRS:
        catalog = json.loads((snap_dir / "catalog.json").read_text())
        for entry in catalog["plugins"]:
            manifest = json.loads(
                (
                    snap_dir
                    / "models"
                    / entry["component"].removeprefix("model-")
                    / "myna-text-model.json"
                ).read_text()
            )
            assert manifest["id"] == entry["model"]
            assert manifest["quality"] == entry["quality"]["value"]
            assert manifest["latency_ms"] == entry["latency_ms"]
            assert manifest["params"] == entry["params"]
            assert manifest["download_bytes"] == entry["download_bytes"]


def test_quality_and_latency_are_never_typed_into_the_source():
    for snap_dir in SNAP_DIRS:
        for path in (snap_dir / "plugins").glob("*.toml"):
            source = tomllib.loads(path.read_text())
            assert {"quality", "latency_ms", "quality_source"}.isdisjoint(source), path


@pytest.mark.parametrize(
    "name", ["GPL-3.0-only", "CC-BY-NC-4.0", "CC-BY-NC-SA-4.0", "Unlicense", ""]
)
def test_a_licence_outside_the_allow_list_fails(valid, name):
    valid["license"] = name
    with pytest.raises(gen.PluginError, match="allow-list"):
        gen.validate(valid)


@pytest.mark.parametrize(
    "name", ["Apache-2.0", "MIT", "MPL-2.0", "CC-BY-4.0", "BSD", "BSD-3-Clause"]
)
def test_the_allowed_licences_pass(valid, name):
    valid["license"] = name
    valid["license_file"] = "/nonexistent"
    gen.validate(valid)


@pytest.mark.parametrize("sha", ["", "abc", "A" * 64, "g" * 64, "a" * 63, "a" * 65, " " + "a" * 63])
def test_a_malformed_sha256_fails(valid, sha):
    valid["files"][0]["sha256"] = sha
    with pytest.raises(gen.PluginError, match="sha256"):
        gen.validate(valid)


@pytest.mark.parametrize(
    "key",
    [
        "id",
        "component",
        "snap",
        "class",
        "needs",
        "license",
        "attribution",
        "source",
        "download_bytes",
        "entry_revision",
        "curated",
        "pin",
        "locales",
        "summary",
        "description",
        "notice",
        "language",
        "task",
        "kind",
    ],
)
def test_a_missing_required_field_fails(valid, key):
    del valid[key]
    with pytest.raises(gen.PluginError, match=key):
        gen.validate(valid)


def test_structural_rules(valid):
    def broken(mutate, match):
        plugin = copy.deepcopy(valid)
        mutate(plugin)
        with pytest.raises(gen.PluginError, match=match):
            gen.validate(plugin)

    broken(lambda p: p.update(component="model-other"), "must be model-")
    broken(lambda p: p.update(component="disfluency-en", name="disfluency-en"), "must be model-")
    broken(lambda p: p.update({"class": "run-shell"}), "class")
    broken(lambda p: p.update(needs="word"), "needs")
    broken(lambda p: p.update(download_bytes=0), "download_bytes")
    broken(lambda p: p.update(entry_revision=0), "entry_revision")
    broken(lambda p: p.update(curated="yes"), "curated")
    broken(lambda p: p.update(locales=[]), "locales")
    broken(lambda p: p.update(languages=["fr", "de"]), "languages")
    broken(lambda p: p.update(languages="en"), "languages")
    broken(lambda p: p["step"].update(stage="anywhere"), "stage")
    broken(lambda p: p["step"].update(id="Bad Id"), "step.id")
    broken(lambda p: p["hardware"].pop("arch"), "hardware")
    broken(lambda p: p.update(files=[]), "files")
    broken(lambda p: p["files"][0].update(bytes=0), "bytes")
    broken(lambda p: p["files"][0].pop("url", None) or p.pop("local"), "no url")
    broken(lambda p: p["local"].pop("env"), r"\[local\]")
    broken(lambda p: p["params"][0].update(kind="number"), "param")
    broken(lambda p: p["measured"].update(quality=1.5), "measured")
    broken(lambda p: p["measured"].update(latency_ms=0), "measured")
    broken(lambda p: p["measured"].update(quality_source=""), "measured")
    broken(lambda p: p.update(license="Unlicense"), "allow-list")
    broken(lambda p: p.update(license="Proprietary", license_file="/x"), "allow-list")


def test_a_licence_without_a_common_text_needs_a_file(valid):
    valid["license"] = "MIT"
    with pytest.raises(gen.PluginError, match="license_file"):
        gen.validate(valid)


def test_latency_classes_follow_the_declared_thresholds():
    assert [gen.latency_class(ms) for ms in (1, 49, 50, 399, 400, 5000)] == [
        "fast",
        "fast",
        "ok",
        "ok",
        "slow",
        "slow",
    ]


def test_a_snap_without_plugins_or_markers_is_an_error(tmp_path):
    snap_dir = tmp_path / "text-snap"
    snap_dir.mkdir()
    with pytest.raises(gen.PluginError, match="no plugins"):
        gen.outputs_of(snap_dir)
    shutil.copytree(SNAP_DIRS[0] / "plugins", snap_dir / "plugins")
    (snap_dir / "snap").mkdir()
    (snap_dir / "snap/snapcraft.yaml").write_text("components:\n")
    with pytest.raises(gen.PluginError, match="markers"):
        gen.outputs_of(snap_dir)


def test_a_plugin_in_the_wrong_snap_directory_is_refused(tmp_path):
    snap_dir = tmp_path / "translation-snap"
    shutil.copytree(SNAP_DIRS[0] / "plugins", snap_dir / "plugins")
    shutil.copytree(SNAP_DIRS[0] / "snap", snap_dir / "snap")
    with pytest.raises(gen.PluginError, match="not this directory's snap"):
        gen.outputs_of(snap_dir)


def test_a_plugin_without_measured_results_is_refused(tmp_path):
    path = tmp_path / "x.toml"
    path.write_text("")
    with pytest.raises(gen.PluginError, match="measured.json"):
        gen.read_plugin(path)


def test_the_staging_plan_names_a_pinned_source_for_every_file():
    for snap_dir in SNAP_DIRS:
        rows = [ln.split("\t") for ln in gen.list_files(snap_dir).splitlines()]
        kinds = {r[0] for r in rows}
        assert kinds == {"P", "F"}
        for row in rows:
            if row[0] == "F":
                assert len(row) == 5 and len(row[3]) == 64
            else:
                assert len(row) == 8 and Path(row[5]).is_absolute()


def test_the_translation_plan_fetches_and_the_tagger_plan_copies_locally():
    text = gen.list_files(SNAP_DIRS[0])
    translation = gen.list_files(SNAP_DIRS[1])
    assert "MYNA_TAGGER_SRC" in text and "https://" not in text
    pairs = sum(row.startswith("P\t") for row in translation.splitlines())
    assert pairs >= 2
    # One url per file: model, lex and a vocabulary, or a source and a target
    # vocabulary for the pairs that split them (en-ja, en-zh-Hans).
    files = sum(len(plugin["files"]) for _, plugin in ALL if plugin["snap"] == "myna-translation")
    assert translation.count("https://firefox-settings-attachments.cdn.mozilla.net/") == files
    assert files >= 3 * pairs


def test_a_component_is_declared_by_exactly_one_source_each():
    for snap_dir in SNAP_DIRS:
        recipe = (snap_dir / "snap/snapcraft.yaml").read_text()
        assert recipe.count(gen.BEGIN) == 2
        assert recipe.count(gen.END) == 2


def gated(valid, **gate):
    plugin = copy.deepcopy(valid)
    plugin["curated"] = True
    plugin["gate"] = {"vendor_comet": 0.87, **gate}
    plugin["measured"] = {**plugin["measured"], "gate": "pass"}
    return plugin


def test_a_plugin_without_a_gate_table_is_listed_as_asked(valid):
    valid["curated"] = True
    assert gen.listed(valid) is True
    valid["curated"] = False
    assert gen.listed(valid) is False


def test_a_gated_plugin_is_listed_on_a_passing_gate_alone(valid):
    plugin = gated(valid)
    assert gen.listed(plugin) is True  # no spot check needed
    plugin["spot_check"] = {"who": "Reviewer", "date": "2026-11-22", "passed": True}
    gen.validate(plugin)
    assert gen.listed(plugin) is True
    plugin["spot_check"]["passed"] = False
    assert gen.listed(plugin) is False  # a failed spot check still unlists
    del plugin["spot_check"]
    plugin["measured"]["gate"] = "fail"
    assert gen.listed(plugin) is False
    plugin["measured"]["runs"] = True  # misses the numbers but serves
    assert gen.listed(plugin) is True
    plugin["measured"]["runs"] = False
    assert gen.listed(plugin) is False
    plugin["measured"]["gate"] = "pass"
    plugin["curated"] = False
    assert gen.listed(plugin) is False


def test_runs_must_be_a_boolean(valid):
    plugin = gated(valid)
    plugin["measured"]["runs"] = "yes"
    with pytest.raises(gen.PluginError, match="runs must be a boolean"):
        gen.validate(plugin)


def test_the_catalog_entry_carries_the_listing_decision(valid):
    plugin = gated(valid)
    assert gen.catalog_entry(plugin)["curated"] is True
    plugin["measured"]["gate"] = "fail"
    assert gen.catalog_entry(plugin)["curated"] is False


@pytest.mark.parametrize(
    ("mutate", "match"),
    [
        (lambda p: p.update(gate="x"), "gate must be a table"),
        (lambda p: p.update(spot_check={"who": "R", "date": "d"}), "spot_check needs"),
        (
            lambda p: p.update(spot_check={"who": "R", "date": "d", "passed": "yes"}),
            "spot_check needs",
        ),
        (
            lambda p: p.update(spot_check={"who": "R", "date": "d", "passed": True, "extra": 1}),
            "spot_check needs",
        ),
        (lambda p: p["measured"].pop("gate"), "from the pipeline"),
        (lambda p: p["measured"].update(gate="maybe"), "from the pipeline"),
    ],
)
def test_malformed_gate_and_spot_check_fields_fail(valid, mutate, match):
    plugin = gated(valid)
    mutate(plugin)
    with pytest.raises(gen.PluginError, match=match):
        gen.validate(plugin)


def test_the_pipeline_and_the_generator_share_one_licence_allow_list():
    from myna.benchmarker import plugin_eval

    assert plugin_eval.LICENSES == gen.LICENSES
    for name in ("BSD", "BSD-2-Clause", "GPL-3.0", "mit"):
        assert plugin_eval.licence_allowed(name) == gen.licence_allowed(name)


def test_the_shipped_gated_plugins_name_the_results_they_came_from():
    for _, plugin in ALL:
        if "gate" in plugin:
            assert plugin["measured"]["gate"] == "pass" or plugin["measured"]["runs"] is True
            assert len(plugin["measured"]["results_sha256"]) == 64


def test_a_plugin_for_several_languages_lists_them_everywhere(valid):
    valid["languages"] = [valid["language"], "fr"]
    gen.validate(valid)
    assert gen.manifest_of(valid)["languages"] == [valid["language"], "fr"]
    entry = gen.catalog_entry(valid)
    assert entry["languages"] == entry["covers"]["language"] == [valid["language"], "fr"]
    assert entry["in_lang"] == valid["language"]
