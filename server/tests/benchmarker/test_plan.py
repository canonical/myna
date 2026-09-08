"""Deciding what to sweep, before anything is installed.

`plan` is the answer to "what is this run going to measure", and it has to be
answerable without root and without snapd - otherwise the only way to see the
matrix is to run it. Everything here reads the source tree: the packed
artefacts, each engine's `engine.yaml`, and the install hook that declares
whether the snap has an emission-mode toggle at all.

The other half is `configs:`, the axis that makes a shipped knob measurable.
Its rules are pinned here because getting them wrong is silent: a mode no entry
claims must still produce exactly one row, and a mode several entries claim must
produce one row each.
"""

from __future__ import annotations

import json

import pytest
import yaml

from myna.benchmarker._run import (
    BATCH,
    STREAMING,
    SnapTarget,
    TargetUnavailable,
    Variant,
    _declared_components,
    _modelctl_command,
    _snap_files,
    cmd_plan,
    declares_streaming,
    engine_options,
    load_config,
    parse_variants,
    variants_for,
)


def make_snap_dir(
    root,
    snap="myna-whisper",
    *,
    components=("model-tiny", "model-base"),
    packed=True,
    pack_components=None,
    apps=None,
    engines=(("cpu", ["tiny", "base"], {"compute-type": "auto"}),),
    streaming=False,
):
    """A source-tree snap directory: packed artefacts, recipe, engines, hook."""
    snap_dir = root / f"{snap}-snap"
    (snap_dir / "snap" / "hooks").mkdir(parents=True)
    recipe = {"components": {c: {"type": "standard"} for c in components}}
    if apps is not None:
        recipe["apps"] = apps
    (snap_dir / "snap" / "snapcraft.yaml").write_text(yaml.safe_dump(recipe), encoding="utf-8")

    hook = "#!/bin/sh\nmodelctl use-engine --auto\n"
    if streaming:
        hook += 'modelctl set --package streaming="true"\n'
    (snap_dir / "snap" / "hooks" / "install").write_text(hook, encoding="utf-8")

    for name, models, configurations in engines:
        engine_dir = snap_dir / "engines" / name
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

    if packed:
        (snap_dir / f"{snap}_0.1.0_amd64.snap").write_bytes(b"")
    for component in components if pack_components is None else pack_components:
        (snap_dir / f"{snap}+{component}.comp").write_bytes(b"")
    return snap_dir


# ─── source-tree artefacts ───────────────────────────────────────────────────


def test_the_packed_snap_and_its_declared_components_are_found(tmp_path):
    snap_dir = make_snap_dir(tmp_path)
    names = [n.rsplit("/", 1)[-1] for n in _snap_files(snap_dir, "myna-whisper")]
    assert names == [
        "myna-whisper_0.1.0_amd64.snap",
        "myna-whisper+model-base.comp",
        "myna-whisper+model-tiny.comp",
    ]


def test_an_undeclared_comp_left_over_from_another_branch_is_ignored(tmp_path):
    """A snap directory accumulates artefacts across branches and renames, and
    installing a component the recipe does not declare fails outright."""
    snap_dir = make_snap_dir(tmp_path)
    (snap_dir / "myna-whisper+qwen-vllm.comp").write_bytes(b"")
    assert not any("vllm" in name for name in _snap_files(snap_dir, "myna-whisper"))


def test_a_declared_component_that_was_never_packed_is_an_error(tmp_path):
    snap_dir = make_snap_dir(tmp_path, pack_components=["model-tiny"])
    with pytest.raises(TargetUnavailable, match="not packed: \\['model-base'\\]"):
        _snap_files(snap_dir, "myna-whisper")


def test_an_unpacked_snap_says_so_rather_than_installing_nothing(tmp_path):
    snap_dir = make_snap_dir(tmp_path, packed=False)
    with pytest.raises(TargetUnavailable, match="pack it first"):
        _snap_files(snap_dir, "myna-whisper")


def test_two_packed_revisions_are_ambiguous_rather_than_arbitrary(tmp_path):
    snap_dir = make_snap_dir(tmp_path)
    (snap_dir / "myna-whisper_0.2.0_amd64.snap").write_bytes(b"")
    with pytest.raises(TargetUnavailable, match="several"):
        _snap_files(snap_dir, "myna-whisper")


def test_components_come_from_the_recipe_not_the_directory(tmp_path):
    snap_dir = make_snap_dir(tmp_path)
    assert _declared_components(snap_dir) == {"model-tiny", "model-base"}


def test_a_directory_with_no_recipe_declares_nothing(tmp_path):
    (tmp_path / "empty").mkdir()
    assert _declared_components(tmp_path / "empty") == set()


# ─── the modelctl command ────────────────────────────────────────────────────


def test_a_cli_app_named_after_the_snap_is_invoked_bare(tmp_path):
    snap_dir = make_snap_dir(tmp_path, apps={"myna-whisper": {}, "server": {"daemon": "simple"}})
    assert _modelctl_command(snap_dir, "myna-whisper") == "myna-whisper"


def test_a_cli_app_named_after_the_adapter_is_invoked_dotted(tmp_path):
    """myna-funasr names its CLI app `funasr`, so the command is
    `myna-funasr.funasr`. Assuming the snap name works for whisper and fails
    for funasr, which is exactly what it did."""
    snap_dir = make_snap_dir(
        tmp_path, snap="myna-funasr", apps={"funasr": {}, "server": {"daemon": "simple"}}
    )
    assert _modelctl_command(snap_dir, "myna-funasr") == "myna-funasr.funasr"


def test_the_daemon_app_is_never_mistaken_for_the_cli(tmp_path):
    snap_dir = make_snap_dir(tmp_path, apps={"server": {"daemon": "simple"}, "whisper": {}})
    assert _modelctl_command(snap_dir, "myna-whisper") == "myna-whisper.whisper"


def test_a_recipe_with_no_apps_falls_back_to_the_snap_name(tmp_path):
    snap_dir = make_snap_dir(tmp_path)
    assert _modelctl_command(snap_dir, "myna-whisper") == "myna-whisper"


# ─── static axes ─────────────────────────────────────────────────────────────


def test_engine_options_report_each_engines_models_and_knobs(tmp_path):
    snap_dir = make_snap_dir(
        tmp_path,
        engines=(
            ("cpu", ["tiny", "base"], {"compute-type": "auto"}),
            ("nvidia-gpu", ["base", "small"], {"compute-type": "float16"}),
        ),
    )
    options = engine_options(snap_dir)
    assert options["cpu"]["models"] == ["tiny", "base"]
    assert options["nvidia-gpu"]["models"] == ["base", "small"]
    assert "compute-type" in options["nvidia-gpu"]["configurations"]


def test_a_snap_with_no_engines_directory_reports_none(tmp_path):
    (tmp_path / "bare").mkdir()
    assert engine_options(tmp_path / "bare") == {}


def test_the_install_hook_is_what_declares_an_emission_toggle(tmp_path):
    """The config key *is* the capability declaration - an adapter with no
    progressive path never sets it - and the hook is its only static record."""
    assert declares_streaming(make_snap_dir(tmp_path / "a", streaming=True)) is True
    assert declares_streaming(make_snap_dir(tmp_path / "b", streaming=False)) is False


def test_a_target_with_no_hook_to_read_reports_unknown(tmp_path):
    (tmp_path / "bare").mkdir()
    assert declares_streaming(tmp_path / "bare") is None


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
        "targets": [{"snap": "myna-whisper", "dir": "myna-whisper-snap"}],
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


def test_relative_paths_resolve_against_the_config_not_the_cwd(tmp_path, monkeypatch):
    """The same config then works from any directory, which is what lets one
    file serve both an in-tree run and a tester's unpacked bundle."""
    make_snap_dir(tmp_path)
    config = write_config(tmp_path / "bench.yaml")
    monkeypatch.chdir(tmp_path.parent)

    cfg = load_config(config, only=None, out_override=None, budget_override=None)

    assert cfg.manifest == tmp_path / "corpus" / "manifest.json"
    assert cfg.out == tmp_path / "results.jsonl"


def test_an_explicit_root_moves_the_base_of_every_relative_path(tmp_path):
    (tmp_path / "conf").mkdir()
    make_snap_dir(tmp_path)
    config = write_config(tmp_path / "conf" / "bench.yaml", root="..")
    cfg = load_config(config, only=None, out_override=None, budget_override=None)
    assert cfg.root == tmp_path
    assert cfg.out == tmp_path / "results.jsonl"


def test_only_narrows_the_target_list(tmp_path):
    make_snap_dir(tmp_path)
    make_snap_dir(tmp_path, snap="myna-sherpa", components=())
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {"snap": "myna-whisper", "dir": "myna-whisper-snap"},
            {"snap": "myna-sherpa", "dir": "myna-sherpa-snap"},
        ],
    )
    cfg = load_config(config, only=["myna-sherpa"], out_override=None, budget_override=None)
    assert [t["snap"] for t in cfg.targets] == ["myna-sherpa"]


def test_narrowing_to_nothing_is_an_error_not_an_empty_sweep(tmp_path):
    make_snap_dir(tmp_path)
    config = write_config(tmp_path / "bench.yaml")
    with pytest.raises(SystemExit, match="no targets selected"):
        load_config(config, only=["myna-qwen"], out_override=None, budget_override=None)


# ─── a source-tree target ────────────────────────────────────────────────────


def test_a_dir_target_derives_its_files_and_cli_from_the_tree(tmp_path):
    make_snap_dir(tmp_path, apps={"whisper": {}, "server": {"daemon": "simple"}})
    target = SnapTarget({"snap": "myna-whisper", "dir": "myna-whisper-snap"}, tmp_path)
    assert target.cli == "myna-whisper.whisper"
    assert len(target.files) == 3  # snap + two components


def test_an_explicit_files_list_wins_over_a_dir(tmp_path):
    """A tester's bundle names artefacts directly; the tree is the in-repo
    convenience, not the authority."""
    make_snap_dir(tmp_path)
    (tmp_path / "downloaded.snap").write_bytes(b"")
    target = SnapTarget(
        {"snap": "myna-whisper", "dir": "myna-whisper-snap", "files": ["downloaded.snap"]},
        tmp_path,
    )
    assert target.files == [str(tmp_path / "downloaded.snap")]


# ─── cmd_plan ────────────────────────────────────────────────────────────────


def test_the_plan_names_every_row_the_sweep_would_produce(tmp_path, capsys):
    make_snap_dir(tmp_path, streaming=True)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "dir": "myna-whisper-snap",
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


def test_a_batch_only_snap_is_planned_without_streaming_rows(tmp_path, capsys):
    make_snap_dir(tmp_path, snap="myna-funasr", components=(), streaming=False)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-funasr", "dir": "myna-funasr-snap"}],
    )
    cmd_plan(PlanArgs(config))
    assert "streaming" not in capsys.readouterr().out


def test_the_models_allowlist_narrows_the_planned_rows(tmp_path, capsys):
    make_snap_dir(tmp_path)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[{"snap": "myna-whisper", "dir": "myna-whisper-snap", "models": ["tiny"]}],
    )
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "/tiny/" in out and "/base/" not in out


def test_an_unpacked_target_is_reported_and_the_rest_still_planned(tmp_path, capsys):
    """A plan that stops at the first unpacked snap hides every target after
    it, which is the half you needed to see."""
    make_snap_dir(tmp_path, snap="myna-qwen", components=(), packed=False)
    make_snap_dir(tmp_path)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {"snap": "myna-qwen", "dir": "myna-qwen-snap"},
            {"snap": "myna-whisper", "dir": "myna-whisper-snap"},
        ],
    )

    cmd_plan(PlanArgs(config))  # not packed is a state of the tree, not a config error

    out = capsys.readouterr().out
    assert "UNAVAILABLE" in out and "pack it first" in out
    assert "myna-whisper/cpu/tiny/batch" in out


def test_a_config_naming_a_key_no_engine_declares_fails_the_plan(tmp_path, capsys):
    """It would take a cell down mid-sweep, hours in. Catch it in the plan."""
    make_snap_dir(tmp_path)
    config = write_config(
        tmp_path / "bench.yaml",
        targets=[
            {
                "snap": "myna-whisper",
                "dir": "myna-whisper-snap",
                "configs": [{"label": "x", "settings": {"not-a-key": "1"}}],
            }
        ],
    )
    with pytest.raises(SystemExit):
        cmd_plan(PlanArgs(config))
    assert "no engine.yaml declares" in capsys.readouterr().out


def test_the_plan_prices_the_sweep_in_wall_clock(tmp_path, capsys):
    make_snap_dir(tmp_path)  # one cpu engine, two models, batch only
    config = write_config(tmp_path / "bench.yaml", sweep_budget_seconds=3600)
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "at most 2 will run here" in out
    assert "2.0 h" in out


def test_the_estimate_counts_one_engine_per_target_not_all_of_them(tmp_path, capsys):
    """Exactly one engine runs. Summing them would quote a machine with an
    NVIDIA card double the sweep it is about to start."""
    make_snap_dir(
        tmp_path,
        engines=(("cpu", ["tiny"], {}), ("nvidia-gpu", ["tiny"], {})),
    )
    config = write_config(tmp_path / "bench.yaml", sweep_budget_seconds=3600)
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "2 row(s) across all engines" in out
    assert "at most 1 will run here" in out
    assert "1.0 h" in out


def test_every_engine_is_planned_and_the_choice_is_named_as_the_machines(tmp_path, capsys):
    """Which engine wins is hardware detection at run time, so the plan is a
    prediction over all of them rather than a guess at one."""
    make_snap_dir(
        tmp_path,
        engines=(("cpu", ["tiny"], {}), ("nvidia-gpu", ["tiny"], {})),
    )
    config = write_config(tmp_path / "bench.yaml")
    cmd_plan(PlanArgs(config))
    out = capsys.readouterr().out
    assert "myna-whisper/cpu/tiny/batch" in out
    assert "myna-whisper/nvidia-gpu/tiny/batch" in out
    assert "one engine is chosen on the machine" in out
