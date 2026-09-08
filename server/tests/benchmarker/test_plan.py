"""Deciding what to sweep, before anything is installed.

`plan` is the answer to "what is this run going to measure", and it has to be
answerable without root and without snapd - otherwise the only way to see the
matrix is to run it. Everything it needs is a property of the packed snap:
which engines it ships, which models each offers, which knobs each declares,
what its CLI app is called, and whether it has an emission-mode toggle at all.

Reading those out of the artefact is what leaves one way to name a target.
There used to be two - `dir:` for a source tree and `files:` for copied
artefacts - and only the first could be planned, so the config we ran here and
the config testers ran were different shapes with different failure modes.

Most of these tests hand `plan` that metadata directly rather than building a
squashfs for it: the extraction is covered on its own below, and everything
else here is about what `plan` does with the answer.

The other half is `configs:`, the axis that makes a shipped knob measurable.
Its rules are pinned here because getting them wrong is silent: a mode no entry
claims must still produce exactly one row, and a mode several entries claim must
produce one row each.
"""

from __future__ import annotations

import json
import shutil
import subprocess

import pytest
import yaml

from myna.benchmarker import _run
from myna.benchmarker._run import (
    BATCH,
    STREAMING,
    Variant,
    _engine_manifests,
    _snap_yaml_command,
    cmd_plan,
    load_config,
    parse_variants,
    variants_for,
)


@pytest.fixture
def snaps(tmp_path, monkeypatch):
    """Build packed artefacts on disk and stub what unsquashing them would say.

    Returns a `make(...)` that writes placeholder `.snap`/`.comp` files and
    registers the metadata for that snap, so a test states its axes in one place
    instead of assembling a filesystem the code only reads back.
    """
    metadata: dict[str, dict] = {}

    def make(
        snap="myna-whisper",
        *,
        version="0.1.0",
        components=("model-tiny", "model-base"),
        engines=(("cpu", ["tiny", "base"], {"compute-type": "auto"}),),
        cli=None,
        streaming=False,
        packed=True,
    ):
        if packed:
            packed_path = tmp_path / f"{snap}_{version}_amd64.snap"
            packed_path.write_bytes(b"")
            metadata[str(packed_path)] = {
                "engines": {
                    name: {"models": list(models), "configurations": dict(configurations)}
                    for name, models, configurations in engines
                },
                "cli": cli or f"{snap}.{snap.split('-', 1)[-1]}",
                "streaming": streaming,
            }
        for component in components:
            (tmp_path / f"{snap}+{component}.comp").write_bytes(b"")
        return tmp_path

    monkeypatch.setattr(_run, "snap_metadata", lambda f: metadata.get(str(f), {}))
    return make


def target_files(snap="myna-whisper"):
    return [f"{snap}_*.snap", f"{snap}+*.comp"]


# ─── reading the packed snap ─────────────────────────────────────────────────


def write_snap_yaml(tmp_path, snap, apps):
    path = tmp_path / "snap.yaml"
    path.write_text(yaml.safe_dump({"name": snap, "apps": apps}), encoding="utf-8")
    return path


def test_a_cli_app_named_after_the_snap_is_invoked_bare(tmp_path, snaps):
    yml = write_snap_yaml(
        tmp_path, "myna-whisper", {"myna-whisper": {}, "server": {"daemon": "simple"}}
    )
    assert _snap_yaml_command(yml) == "myna-whisper"


def test_a_cli_app_named_after_the_adapter_is_invoked_dotted(tmp_path, snaps):
    """myna-funasr names its CLI app `funasr`, so the command is
    `myna-funasr.funasr`. Assuming the snap name works for whisper and fails
    for funasr, which is exactly what it did."""
    yml = write_snap_yaml(tmp_path, "myna-funasr", {"funasr": {}, "server": {"daemon": "simple"}})
    assert _snap_yaml_command(yml) == "myna-funasr.funasr"


def test_the_daemon_app_is_never_mistaken_for_the_cli(tmp_path, snaps):
    yml = write_snap_yaml(tmp_path, "myna-whisper", {"server": {"daemon": "simple"}, "whisper": {}})
    assert _snap_yaml_command(yml) == "myna-whisper.whisper"


def test_a_snap_with_no_apps_at_all_names_no_command(tmp_path, snaps):
    """The target then falls back to the snap name, which is what it did
    before any of this was read."""
    assert _snap_yaml_command(write_snap_yaml(tmp_path, "myna-whisper", {})) is None
    assert _snap_yaml_command(tmp_path / "absent.yaml") is None


def test_engine_manifests_report_each_engines_models_and_knobs(tmp_path, snaps):
    for name, models, configurations in (
        ("cpu", ["tiny", "base"], {"compute-type": "auto"}),
        ("nvidia-gpu", ["base", "small"], {"compute-type": "float16"}),
    ):
        engine_dir = tmp_path / "engines" / name
        engine_dir.mkdir(parents=True)
        (engine_dir / "engine.yaml").write_text(
            yaml.safe_dump(
                {
                    "name": name,
                    "model": {"default": models[0], "options": models},
                    "configurations": configurations,
                }
            ),
            encoding="utf-8",
        )
    options = _engine_manifests(tmp_path / "engines")
    assert options["cpu"]["models"] == ["tiny", "base"]
    assert options["nvidia-gpu"]["models"] == ["base", "small"]
    assert "compute-type" in options["nvidia-gpu"]["configurations"]


def test_a_snap_with_no_engines_directory_reports_none(tmp_path, snaps):
    assert _engine_manifests(tmp_path / "absent") == {}


# ─── configs: ────────────────────────────────────────────────────────────────


def test_a_config_entry_defaults_to_both_modes():
    (variant,) = parse_variants(
        {"configs": [{"label": "int8", "settings": {"compute-type": "int8"}}]}, "s"
    )
    assert variant == Variant("int8", (BATCH, STREAMING), {"compute-type": "int8"})


def test_settings_are_stringified_so_yaml_ints_survive_the_cli():
    (variant,) = parse_variants(
        {"configs": [{"label": "arm3s", "settings": {"stream-arm-seconds": 3}}]}, "s"
    )
    assert variant.settings == {"stream-arm-seconds": "3"}


def test_a_config_entry_needs_a_label():
    with pytest.raises(SystemExit, match="needs a label"):
        parse_variants({"configs": [{"settings": {"a": "1"}}]}, "s")


def test_two_config_entries_may_not_share_a_label():
    """Labels become the row's identity, and the summary dedups by label - two
    rows called the same thing silently become one."""
    entries = [{"label": "x", "settings": {"a": "1"}}, {"label": "x", "settings": {"a": "2"}}]
    with pytest.raises(SystemExit, match="duplicate config label"):
        parse_variants({"configs": entries}, "s")


def test_a_config_entry_with_no_settings_is_not_a_row():
    with pytest.raises(SystemExit, match="need settings"):
        parse_variants({"configs": [{"label": "x"}]}, "s")


def test_an_unknown_mode_is_rejected():
    with pytest.raises(SystemExit, match="unknown mode"):
        parse_variants(
            {"configs": [{"label": "x", "modes": ["turbo"], "settings": {"a": "1"}}]}, "s"
        )


def test_streaming_may_not_be_smuggled_in_as_a_setting():
    """It is the mode axis. Setting it here would put the label and the snap's
    actual emission mode permanently out of step."""
    entry = {"label": "x", "modes": ["batch"], "settings": {"streaming": "true"}}
    with pytest.raises(SystemExit, match="is the mode axis"):
        parse_variants({"configs": [entry]}, "s")


def test_the_old_streaming_configs_spelling_is_refused_with_the_rewrite():
    with pytest.raises(SystemExit, match="replaced by 'configs'"):
        parse_variants({"streaming_configs": [{"label": "arm3s", "settings": {}}]}, "s")


def test_a_mode_no_config_claims_still_gets_exactly_one_row():
    """Otherwise adding a streaming-only dial would silently delete every batch
    row from the matrix."""
    variants = [Variant("arm3s", (STREAMING,), {"a": "1"})]
    assert variants_for(variants, BATCH) == [None]
    assert variants_for(variants, STREAMING) == variants


def test_a_mode_several_configs_claim_gets_one_row_each():
    variants = [
        Variant("int8", (BATCH, STREAMING), {"compute-type": "int8"}),
        Variant("float32", (BATCH,), {"compute-type": "float32"}),
    ]
    assert [v.label for v in variants_for(variants, BATCH)] == ["int8", "float32"]
    assert [v.label for v in variants_for(variants, STREAMING)] == ["int8"]


def test_no_configs_at_all_is_one_row_per_mode():
    assert variants_for([], BATCH) == [None]
    assert variants_for([], STREAMING) == [None]


# ─── load_config ─────────────────────────────────────────────────────────────


def write_config(path, **overrides):
    cfg = {
        "manifest": "corpus/manifest.json",
        "out": "results.jsonl",
        "targets": [{"snap": "myna-whisper", "files": target_files()}],
    }
    cfg.update(overrides)
    path.write_text(json.dumps(cfg), encoding="utf-8")
    return path


class PlanArgs:
    def __init__(self, config, only=None, out=None, budget=None, label_suffix=""):
        self.config = str(config)
        self.only = only
        self.out = out
        self.budget = budget
        self.label_suffix = label_suffix


def test_relative_paths_resolve_against_the_config_not_the_cwd(tmp_path, snaps, monkeypatch):
    """The same config then works from any directory, which is what lets one
    file serve both an in-tree run and a tester's unpacked bundle."""
    snaps()
    config = write_config(tmp_path / "bench.yaml")
    monkeypatch.chdir(tmp_path.parent)

    cfg = load_config(config, only=None, out_override=None, budget_override=None)

    assert cfg.manifest == tmp_path / "corpus" / "manifest.json"
    assert cfg.out == tmp_path / "results.jsonl"


def test_an_explicit_root_moves_the_base_of_every_relative_path(tmp_path, snaps):
    (tmp_path / "conf").mkdir()
    snaps()
    config = write_config(tmp_path / "conf" / "bench.yaml", root="..")
    cfg = load_config(config, only=None, out_override=None, budget_override=None)
    assert cfg.root == tmp_path
    assert cfg.out == tmp_path / "results.jsonl"


def test_only_narrows_the_target_list(tmp_path, snaps):
    snaps()
    snaps(snap="myna-sherpa", components=())
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {"snap": "myna-whisper", "files": target_files()},
            {"snap": "myna-sherpa", "files": ["myna-sherpa_*.snap"]},
        ],
    )
    cfg = load_config(config, only=["myna-sherpa"], out_override=None, budget_override=None)
    assert [t["snap"] for t in cfg.targets] == ["myna-sherpa"]


def test_narrowing_to_nothing_is_an_error_not_an_empty_sweep(tmp_path, snaps):
    snaps()
    config = write_config(tmp_path / "bench.yaml")
    with pytest.raises(SystemExit, match="no targets selected"):
        load_config(config, only=["myna-qwen"], out_override=None, budget_override=None)


# ─── cmd_plan ────────────────────────────────────────────────────────────────


def test_the_plan_names_every_row_the_sweep_would_produce(tmp_path, snaps, capsys):
    snaps(streaming=True)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "files": target_files(),
                "configs": [
                    {"label": "int8", "modes": ["batch"], "settings": {"compute-type": "int8"}}
                ],
            }
        ],
    )

    cmd_plan(PlanArgs(config))

    out = capsys.readouterr().out
    assert "myna-whisper/cpu/tiny/batch-int8" in out
    assert "myna-whisper/cpu/tiny/streaming" in out
    assert "myna-whisper/cpu/base/batch-int8" in out
    assert "4 row(s)" in out  # 2 models x (1 batch config + 1 default streaming)


def test_the_cli_command_comes_from_the_snap_not_the_config(tmp_path, snaps, capsys):
    """It is named after the adapter, not the snap, and spelling it out by hand
    was the one thing every tester's config got wrong."""
    snaps(snap="myna-funasr", components=(), engines=(("cpu", ["sensevoice"], {}),))
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-funasr", "files": ["myna-funasr_*.snap"]}],
    )
    cmd_plan(PlanArgs(config))
    assert "cli=myna-funasr.funasr" in capsys.readouterr().out


def test_a_batch_only_snap_is_planned_without_streaming_rows(tmp_path, snaps, capsys):
    snaps(snap="myna-funasr", components=(), engines=(("cpu", ["sensevoice"], {}),))
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-funasr", "files": ["myna-funasr_*.snap"]}],
    )
    cmd_plan(PlanArgs(config))
    assert "streaming" not in capsys.readouterr().out


def test_the_models_allowlist_narrows_the_planned_rows(tmp_path, snaps, capsys):
    snaps()
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-whisper", "files": target_files(), "models": ["tiny"]}],
    )
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "/tiny/" in out and "/base/" not in out


def test_an_unpacked_target_is_reported_and_the_rest_still_planned(tmp_path, snaps, capsys):
    """A plan that stops at the first unpacked snap hides every target after
    it, which is the half you needed to see."""
    snaps()
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {"snap": "myna-qwen", "files": ["myna-qwen_*.snap"]},
            {"snap": "myna-whisper", "files": target_files()},
        ],
    )

    cmd_plan(PlanArgs(config))  # not packed is a state of the tree, not a config error

    out = capsys.readouterr().out
    assert "UNAVAILABLE" in out and "pack it first" in out
    assert "myna-whisper/cpu/tiny/batch" in out


# ─── artefact-only targets ───────────────────────────────────────────────────


def make_packed_snap(tmp_path, snap="myna-whisper", engines=(("cpu", ["tiny"], {}),)):
    """A real squashfs carrying the three files a source tree is read for."""
    stage = tmp_path / "stage"
    (stage / "meta" / "hooks").mkdir(parents=True)
    (stage / "meta" / "snap.yaml").write_text(
        yaml.safe_dump(
            {
                "name": snap,
                "apps": {"whisper": {"command": "bin/whisper"}, "server": {"daemon": "simple"}},
            }
        ),
        encoding="utf-8",
    )
    (stage / "meta" / "hooks" / "install").write_text(
        'modelctl set --package streaming="false"\n', encoding="utf-8"
    )
    for name, models, configurations in engines:
        engine_dir = stage / "engines" / name
        engine_dir.mkdir(parents=True)
        (engine_dir / "engine.yaml").write_text(
            yaml.safe_dump(
                {
                    "name": name,
                    "model": {"default": models[0], "options": models},
                    "configurations": configurations,
                }
            ),
            encoding="utf-8",
        )
    packed = tmp_path / f"{snap}_0.1.0_amd64.snap"
    subprocess.run(
        ["mksquashfs", str(stage), str(packed), "-noappend", "-quiet", "-no-progress"],
        check=True,
        capture_output=True,
    )
    return packed


@pytest.mark.skipif(
    shutil.which("mksquashfs") is None or shutil.which("unsquashfs") is None,
    reason="squashfs-tools not installed",
)
def test_a_target_given_as_artefacts_is_planned_from_the_snap_itself(tmp_path, capsys):
    """A tester's config lists files, not a source tree. Reading the .snap is
    what lets `plan` answer for it at all, rather than saying "unknown" and
    leaving a bad key to take a cell down hours in."""
    packed = make_packed_snap(tmp_path, engines=(("cpu", ["tiny", "base"], {}),))
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-whisper", "files": [packed.name]}],
    )
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "myna-whisper/cpu/tiny/batch" in out
    assert "myna-whisper/cpu/base/batch" in out
    # And the CLI app name, which the snap name is not.
    assert "cli=myna-whisper.whisper" in out


@pytest.mark.skipif(
    shutil.which("mksquashfs") is None or shutil.which("unsquashfs") is None,
    reason="squashfs-tools not installed",
)
def test_a_bad_key_in_an_artefact_only_target_fails_the_plan(tmp_path, capsys):
    packed = make_packed_snap(tmp_path, engines=(("cpu", ["tiny"], {"compute-type": "auto"}),))
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "files": [packed.name],
                "configs": [{"label": "x", "settings": {"not-a-key": "1"}}],
            }
        ],
    )
    with pytest.raises(SystemExit):
        cmd_plan(PlanArgs(config))
    assert "not declared by engine 'cpu'" in capsys.readouterr().out


# ─── explicit values, explicit engines ───────────────────────────────────────


@pytest.mark.parametrize("value", ["auto", "default", "AUTO"])
def test_a_config_point_that_defers_the_choice_is_refused(value):
    """ "auto" is not a setting, it is a request for someone else to decide - and
    whisper's resolves per model, so one auto row is int8 on tiny and float32 on
    base under a single name. Both were measured that way."""
    with pytest.raises(SystemExit, match="defers the choice"):
        parse_variants(
            {"configs": [{"label": "auto", "settings": {"compute-type": value}}]}, "myna-whisper"
        )


def test_a_config_point_can_be_scoped_to_the_engines_it_makes_sense_on():
    variants = parse_variants(
        {
            "configs": [
                {
                    "label": "fp16",
                    "engines": ["nvidia-gpu"],
                    "settings": {"compute-type": "float16"},
                },
                {"label": "int8", "engines": ["cpu"], "settings": {"compute-type": "int8"}},
            ]
        },
        "myna-whisper",
    )
    assert [v.label for v in variants_for(variants, BATCH, "cpu")] == ["int8"]
    assert [v.label for v in variants_for(variants, BATCH, "nvidia-gpu")] == ["fp16"]


def test_an_engine_whose_points_are_all_scoped_elsewhere_still_gets_one_row():
    """Otherwise a config written for a GPU box drops the CPU target entirely."""
    variants = parse_variants(
        {
            "configs": [
                {
                    "label": "fp16",
                    "engines": ["nvidia-gpu"],
                    "settings": {"compute-type": "float16"},
                }
            ]
        },
        "myna-whisper",
    )
    assert variants_for(variants, BATCH, "cpu") == [None]


def test_a_key_missing_from_one_engine_is_fine_when_the_point_is_scoped_to_another(
    tmp_path, snaps, capsys
):
    snaps(
        engines=(
            ("cpu", ["tiny"], {"sleep-idle-seconds": "300"}),
            ("nvidia-gpu", ["tiny"], {"compute-type": "float16"}),
        ),
    )
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "files": target_files(),
                "configs": [
                    {
                        "label": "fp16",
                        "engines": ["nvidia-gpu"],
                        "settings": {"compute-type": "float16"},
                    }
                ],
            }
        ],
    )
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "myna-whisper/nvidia-gpu/tiny/batch-fp16" in out
    assert "myna-whisper/cpu/tiny/batch" in out


def test_naming_an_engine_the_snap_does_not_ship_fails_the_plan(tmp_path, snaps, capsys):
    snaps()
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-whisper", "files": target_files(), "engines": ["nvidia-gpu"]}],
    )
    with pytest.raises(SystemExit):
        cmd_plan(PlanArgs(config))
    assert "this snap ships ['cpu']" in capsys.readouterr().out


def test_named_engines_are_all_counted_where_auto_selection_counts_only_the_widest(
    tmp_path, snaps, capsys
):
    """One machine runs one auto-selection, so an unnamed target is quoted its
    largest engine; a target that names both actually measures both."""
    engines = (("cpu", ["tiny"], {}), ("nvidia-gpu", ["tiny"], {}))
    snaps(engines=engines)
    auto = write_config(tmp_path / "auto.yaml", sweep_budget_seconds=3600)
    cmd_plan(PlanArgs(auto))
    assert "at most 1 will run here." in capsys.readouterr().out

    both = write_config(
        tmp_path / "both.yaml",
        sweep_budget_seconds=3600,
        targets=[
            {
                "snap": "myna-whisper",
                "files": target_files(),
                "engines": ["cpu", "nvidia-gpu"],
            }
        ],
    )
    cmd_plan(PlanArgs(both))
    assert "at most 2 will run here." in capsys.readouterr().out


def test_a_config_naming_a_key_no_engine_declares_fails_the_plan(tmp_path, snaps, capsys):
    """It would take a cell down mid-sweep, hours in. Catch it in the plan."""
    snaps()
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "files": target_files(),
                "configs": [{"label": "x", "settings": {"not-a-key": "1"}}],
            }
        ],
    )
    with pytest.raises(SystemExit):
        cmd_plan(PlanArgs(config))
    assert "not declared by engine 'cpu'" in capsys.readouterr().out


def test_the_plan_prices_the_sweep_in_wall_clock(tmp_path, snaps, capsys):
    snaps()  # one cpu engine, two models, batch only
    config = write_config(tmp_path / "bench.yaml", sweep_budget_seconds=3600)
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "at most 2 will run here" in out
    assert "2.0 h" in out


def test_the_estimate_counts_one_engine_per_target_not_all_of_them(tmp_path, snaps, capsys):
    """Exactly one engine runs. Summing them would quote a machine with an
    NVIDIA card double the sweep it is about to start."""
    snaps(
        engines=(("cpu", ["tiny"], {}), ("nvidia-gpu", ["tiny"], {})),
    )
    config = write_config(tmp_path / "bench.yaml", sweep_budget_seconds=3600)
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "2 row(s) across all engines" in out
    assert "at most 1 will run here" in out
    assert "1.0 h" in out


def test_every_engine_is_planned_and_the_choice_is_named_as_the_machines(tmp_path, snaps, capsys):
    """Which engine wins is hardware detection at run time, so the plan is a
    prediction over all of them rather than a guess at one."""
    snaps(
        engines=(("cpu", ["tiny"], {}), ("nvidia-gpu", ["tiny"], {})),
    )
    config = write_config(tmp_path / "bench.yaml")
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "myna-whisper/cpu/tiny/batch" in out
    assert "myna-whisper/nvidia-gpu/tiny/batch" in out
    assert "one engine is chosen on the machine" in out
