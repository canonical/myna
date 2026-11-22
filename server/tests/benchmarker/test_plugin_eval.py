"""The plugin pipeline: refusals, fetch, staging, the daemon and the results."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import socket
import stat
import sys
from pathlib import Path

import pytest

from myna.benchmarker import plugin_eval as pe
from myna.benchmarker import text_suite
from myna.benchmarker.text_gate import GateConfig, GateError

HERE = Path(__file__).resolve().parent
FAKE = HERE.parent / "text" / "fake_bergamot.py"
FILES = {"model.bin": b"model-bytes", "vocab.spm": b"vocab-bytes"}

TOML = """\
id = "fake-en-fr"
component = "model-translate-en-fr"
snap = "myna-translation"
kind = "translate"
task = "text.translate"
class = "translate"
language = "en"
to = "fr"
license = "MPL-2.0"
pin = "fake 1"
{extra}
[[files]]
name = "model.bin"
sha256 = "{m_sha}"
bytes = {m_len}
url = "{m_url}"

[[files]]
name = "vocab.spm"
sha256 = "{v_sha}"
bytes = {v_len}
url = "{v_url}"
"""


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_repo(
    tmp_path: Path, extra: str = "[gate]\nvendor_comet = 0.87\nchrf_min = 0.9\n"
) -> tuple[Path, Path]:
    """A tiny repo tree with one translation plugin and its generated outputs."""
    src = tmp_path / "upstream"
    src.mkdir()
    for name, data in FILES.items():
        (src / name).write_bytes(data)
    repo = tmp_path / "repo"
    snap = repo / "translation-snap"
    (snap / "plugins").mkdir(parents=True)
    toml = TOML.format(
        extra=extra,
        m_sha=sha(FILES["model.bin"]),
        m_len=len(FILES["model.bin"]),
        m_url=(src / "model.bin").as_uri(),
        v_sha=sha(FILES["vocab.spm"]),
        v_len=len(FILES["vocab.spm"]),
        v_url=(src / "vocab.spm").as_uri(),
    )
    (snap / "plugins" / "translate-en-fr.toml").write_text(toml, encoding="utf-8")
    models = snap / "models" / "translate-en-fr"
    models.mkdir(parents=True)
    notice = "Notice.\n" + "".join(f"  {n}  {sha(d)}\n" for n, d in FILES.items())
    (models / "NOTICE").write_text(notice, encoding="utf-8")
    manifest = {
        "kind": "translate",
        "id": "fake-en-fr",
        "task": "text.translate",
        "language": "en",
        "to": "fr",
        "latency_ms": 25,
        "quality": 0.7,
        "config": "config.yml",
    }
    (models / "myna-text-model.json").write_text(json.dumps(manifest), encoding="utf-8")
    (models / "config.yml").write_text("beam-size: 1\n", encoding="utf-8")
    fake = tmp_path / "myna-bergamot"
    fake.write_text(
        f"#!{sys.executable}\nimport runpy\nrunpy.run_path({str(FAKE)!r}, run_name='__main__')\n"
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    return repo, fake


@pytest.fixture
def env(tmp_path, monkeypatch):
    repo, fake = make_repo(tmp_path)
    monkeypatch.setenv("MYNA_REPO_ROOT", str(repo))
    monkeypatch.setenv("MYNA_PIPELINE_HOST", socket.gethostname())
    return repo, fake


def args_for(tmp_path, fake, **over):
    values = {
        "plugin": "fake-en-fr",
        "bergamot": str(fake),
        "cache": str(tmp_path / "cache"),
        "results_dir": str(tmp_path / "results"),
        "repeats": 1,
        "no_measured": False,
    }
    return argparse.Namespace(**{**values, **over})


def plugin_of(repo):
    from myna.benchmarker.text_gate import load_plugin

    return load_plugin(repo / "translation-snap/plugins/translate-en-fr.toml")


def test_a_licence_off_the_allow_list_is_refused(env):
    repo, _ = env
    plugin = plugin_of(repo) | {"license": "CC-BY-NC-4.0"}
    with pytest.raises(pe.PipelineError, match="licence 'CC-BY-NC-4.0' is not on the allow-list"):
        pe.check_inputs(plugin, repo / "translation-snap")
    del plugin["license"]
    with pytest.raises(pe.PipelineError, match="licence None is not on the allow-list"):
        pe.check_inputs(plugin, repo / "translation-snap")
    for ok in ("Apache-2.0", "MIT", "MPL-2.0", "CC-BY-4.0", "BSD-3-Clause", "BSD"):
        assert pe.licence_allowed(ok)
    for bad in ("GPL-3.0", "", "CC-BY-NC-4.0", "BSD-9-Clause-X", "mit"):
        assert not pe.licence_allowed(bad)


def test_a_missing_empty_or_stale_notice_is_refused(env):
    repo, _ = env
    snap = repo / "translation-snap"
    plugin = plugin_of(repo)
    assert pe.check_inputs(plugin, snap) == snap / "models/translate-en-fr"
    notice = snap / "models/translate-en-fr/NOTICE"
    notice.write_text("  ", encoding="utf-8")
    with pytest.raises(pe.PipelineError, match="missing NOTICE"):
        pe.check_inputs(plugin, snap)
    notice.write_text("Notice with no hashes\n", encoding="utf-8")
    with pytest.raises(pe.PipelineError, match="does not list model.bin"):
        pe.check_inputs(plugin, snap)
    notice.unlink()
    with pytest.raises(pe.PipelineError, match="missing NOTICE"):
        pe.check_inputs(plugin, snap)
    notice.write_text("".join(sha(d) for d in FILES.values()), encoding="utf-8")
    (snap / "models/translate-en-fr/myna-text-model.json").unlink()
    with pytest.raises(pe.PipelineError, match="no myna-text-model.json"):
        pe.check_inputs(plugin, snap)


def test_fetch_downloads_verifies_and_reuses_the_cache(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo)
    cache = tmp_path / "cache"
    files = pe.fetch_files(plugin, cache)
    assert {n: p.read_bytes() for n, p in files.items()} == FILES
    for entry in plugin["files"]:
        Path(entry["url"].removeprefix("file://")).unlink()
    assert pe.fetch_files(plugin, cache).keys() == FILES.keys()


def test_fetch_refuses_and_deletes_a_file_that_differs_from_its_pin(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo)
    plugin["files"][0]["sha256"] = "0" * 64
    cache = tmp_path / "cache"
    with pytest.raises(pe.PipelineError, match="model.bin: does not match the sha256"):
        pe.fetch_files(plugin, cache)
    assert not (cache / "model.bin").exists()


def test_fetch_refuses_a_size_that_differs_and_a_file_with_no_source(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo)
    plugin["files"][0]["bytes"] += 1
    with pytest.raises(pe.PipelineError, match="does not match"):
        pe.fetch_files(plugin, tmp_path / "c1")
    plugin = plugin_of(repo)
    del plugin["files"][1]["url"]
    with pytest.raises(pe.PipelineError, match="vocab.spm: not cached and has no url"):
        pe.fetch_files(plugin, tmp_path / "c2")


def test_a_corrupt_cached_file_is_fetched_again(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo)
    cache = tmp_path / "cache"
    cache.mkdir()
    (cache / "model.bin").write_bytes(b"truncated")
    assert pe.fetch_files(plugin, cache)["model.bin"].read_bytes() == FILES["model.bin"]


def test_stage_lays_out_the_component_like_the_snap(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo)
    files = pe.fetch_files(plugin, tmp_path / "cache")
    components = tmp_path / "components"
    components.mkdir()
    out = pe.stage_component(
        plugin, repo / "translation-snap/models/translate-en-fr", files, components
    )
    assert out == components / "model-translate-en-fr"
    names = {p.name for p in out.iterdir()}
    assert names >= {"model.bin", "vocab.spm", "NOTICE", "config.yml", "myna-text-model.json"}
    assert (out / "model.bin").read_bytes() == FILES["model.bin"]
    licence = Path("/usr/share/common-licenses/MPL-2.0")
    assert ("LICENSE" in names) == licence.is_file()


def test_stage_takes_an_explicit_licence_file(env, tmp_path):
    repo, _ = env
    plugin = plugin_of(repo) | {"license_file": str(tmp_path / "L.txt")}
    (tmp_path / "L.txt").write_text("licence text", encoding="utf-8")
    components = tmp_path / "components"
    components.mkdir()
    out = pe.stage_component(
        plugin,
        repo / "translation-snap/models/translate-en-fr",
        pe.fetch_files(plugin, tmp_path / "c"),
        components,
    )
    assert (out / "LICENSE").read_text(encoding="utf-8") == "licence text"


def test_stage_hardlinks_on_one_filesystem_and_copies_across_devices(env, tmp_path, monkeypatch):
    repo, _ = env
    plugin = plugin_of(repo)
    files = pe.fetch_files(plugin, tmp_path / "cache")
    models = repo / "translation-snap/models/translate-en-fr"
    one = tmp_path / "one"
    one.mkdir()
    linked = pe.stage_component(plugin, models, files, one)
    assert (linked / "model.bin").samefile(files["model.bin"])
    monkeypatch.setattr(pe, "_same_fs", lambda a, b: False)
    two = tmp_path / "two"
    two.mkdir()
    copied = pe.stage_component(plugin, models, files, two)
    assert (copied / "model.bin").read_bytes() == FILES["model.bin"]
    assert not (copied / "model.bin").samefile(files["model.bin"])


def test_the_snap_binary_is_the_last_resort(monkeypatch, tmp_path):
    monkeypatch.delenv("MYNA_BERGAMOT", raising=False)
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setattr(pe, "SNAP_BERGAMOT", tmp_path / "absent")
    assert pe._find_bergamot() is None
    present = tmp_path / "present"
    present.write_text("")
    monkeypatch.setattr(pe, "SNAP_BERGAMOT", present)
    assert pe._find_bergamot() == present


def test_peak_rss_is_the_status_figure_in_megabytes():
    import subprocess

    child = subprocess.Popen(
        [sys.executable, "-c", "import time; print('up', flush=True); time.sleep(30)"],
        stdout=subprocess.PIPE,
    )
    try:
        assert child.stdout.readline() == b"up\n"
        status = Path(f"/proc/{child.pid}/status").read_text().splitlines()
        kb = next(int(ln.split()[1]) for ln in status if ln.startswith("VmHWM"))
        assert pe.peak_rss_mb([child.pid]) == kb / 1024
        assert pe.peak_rss_mb([child.pid, child.pid]) == 2 * kb / 1024
    finally:
        child.kill()
        child.wait()


def test_process_tree_and_rss_read_proc_for_this_process_and_its_children():
    import subprocess

    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    try:
        tree = pe.process_tree(os.getpid())
        assert os.getpid() in tree and child.pid in tree
        assert pe.process_tree(child.pid) == [child.pid]
        assert pe.peak_rss_mb([child.pid]) > 1
        assert pe.peak_rss_mb([child.pid, 2**30]) == pytest.approx(
            pe.peak_rss_mb([child.pid]), rel=0.2
        )
        assert pe.peak_rss_mb([]) == 0.0
    finally:
        child.kill()
        child.wait()


@pytest.mark.parametrize(
    ("extra", "state"),
    [
        ({}, "missing"),
        ({"gate": {"spot_check_waiver": "shipped before the gate"}}, "waived"),
        ({"spot_check": {"who": "A", "date": "2026-11-22", "passed": True}}, "passed"),
        ({"spot_check": {"who": "A", "date": "2026-11-22", "passed": False}}, "failed"),
        (
            {
                "gate": {"spot_check_waiver": "x"},
                "spot_check": {"who": "A", "date": "2026-11-22", "passed": False},
            },
            "failed",
        ),
    ],
)
def test_spot_check_state(extra, state):
    assert pe.spot_check_state(extra) == state


@pytest.mark.parametrize(
    "spot",
    [
        {"who": "A", "date": "d"},
        {"who": "", "date": "d", "passed": True},
        {"date": "d", "passed": True},
    ],
)
def test_an_incomplete_spot_check_is_refused(spot):
    with pytest.raises(pe.PipelineError, match="who, date and passed"):
        pe.spot_check_state({"spot_check": spot})


def test_quality_source_names_the_vendor_evidence_or_its_absence():
    gate = GateConfig(vendor_comet=0.8697)
    assert pe.quality_source(gate, 0.7012, 200, "translate_en_fr") == (
        "chrF 0.701 on 200 FLEURS pairs (translate_en_fr); vendor FLORES COMET 0.8697"
    )
    assert pe.quality_source(GateConfig(), 0.5, 200, "translate_en_xx").endswith(
        "quality not measured by the vendor"
    )
    assert pe.quality_source(GateConfig(), None, 0, "x") == "not measured"


def report_of(gates, detail):
    report = text_suite.Report("s")
    report.gates = gates
    report.detail = detail
    return report


def translate_report(status="pass", chrf=0.7012, p95=18.2):
    gates = [text_suite.Gate("translate_en_fr", "chrf", chrf, 0.58, ">=", status)]
    gates.append(text_suite.Gate("translate_en_fr", "p95_ms", p95, 400, "<", "pass"))
    detail = {"translate_en_fr": {"n": 200, "latency_ms": {"p50": 10.0, "p95": p95}}}
    return report_of(gates, detail)


PLUGIN = {"id": "p", "component": "c", "snap": "s", "task": "text.translate"}


def test_results_say_pass_only_when_every_gate_passed():
    ok = pe.build_results(
        PLUGIN, translate_report(), ["translate_en_fr"], GateConfig(vendor_comet=0.87), {"x": 1}
    )
    assert ok["gate"] == "pass" and ok["spot_check"] == "missing" and ok["x"] == 1
    assert ok["measured"] == {
        "chrf": 0.7012,
        "latency_ms": {"p50": 10.0, "p95": 18.2},
        "n": 200,
        "vendor_comet": 0.87,
    }
    for status in ("fail", "pending"):
        bad = pe.build_results(PLUGIN, translate_report(status), ["translate_en_fr"], None, {})
        assert bad["gate"] == "fail" and bad["measured"]["vendor_comet"] is None


def test_measured_json_rounds_latency_up_to_a_step_and_records_the_gate_and_results_hash():
    gate = GateConfig(vendor_comet=0.87)
    results = pe.build_results(PLUGIN, translate_report(), ["translate_en_fr"], gate, {})
    slim = pe.measured_json(results, gate, ["translate_en_fr"], "abc")
    assert slim == {
        "latency_ms": 20,
        "quality": 0.701,
        "quality_source": "chrF 0.701 on 200 FLEURS pairs (translate_en_fr); vendor FLORES COMET 0.87",
        "gate": "pass",
        "results_sha256": "abc",
    }
    exact = pe.build_results(PLUGIN, translate_report(p95=20.0), ["translate_en_fr"], gate, {})
    assert pe.measured_json(exact, gate, ["translate_en_fr"], "")["latency_ms"] == 20
    fast = translate_report(p95=0.2)
    results = pe.build_results(PLUGIN, fast, ["translate_en_fr"], gate, {})
    assert pe.measured_json(results, gate, ["translate_en_fr"], "")["latency_ms"] == 5


def test_measured_json_of_a_cleanup_plugin_reports_reduction_and_uer():
    plugin = PLUGIN | {"task": "text.cleanup"}
    gates = [
        text_suite.Gate("clean_en", "uer", 0.005, 0.01, "<=", "pass"),
        text_suite.Gate("disfluent_en", "reduction", 0.8123, 0.5, ">=", "pass"),
    ]
    detail = {
        "clean_en": {"latency_ms": {"p95": 4.2}},
        "disfluent_en": {"latency_ms": {"p95": 6.1}},
    }
    suites = ["clean_en", "disfluent_en"]
    results = pe.build_results(plugin, report_of(gates, detail), suites, None, {})
    assert results["measured"] == {}
    slim = pe.measured_json(results, None, suites, "h")
    assert slim["latency_ms"] == 10 and slim["quality"] == 0.812
    assert slim["quality_source"] == "cleanup reduction 0.812, UER 0.0050 on the text corpus"


def test_the_pipeline_runs_through_the_real_daemon_and_writes_both_files(env, tmp_path, capsys):
    repo, fake = env
    results = pe.run(args_for(tmp_path, fake))
    out = Path(results["results_file"])
    assert out == tmp_path / "results/plugins/fake-en-fr.json"
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["plugin"] == "fake-en-fr" and saved["schema"] == pe.SCHEMA
    assert (saved["component"], saved["snap"]) == ("model-translate-en-fr", "myna-translation")
    assert saved["gate"] == "fail"  # an upper-casing stand-in is not a translator
    hyps = saved["detail"]["translate_en_fr"]["hypotheses"]
    assert len(hyps) == 200 and hyps[0]["hypothesis"] == hyps[0]["source"].upper()
    assert saved["rss_mb"] > 1 and saved["rss_mb"] == round(saved["rss_mb"], 1)
    assert saved["repeats"] == 1 and re.fullmatch(r"\d{4}-\d{2}-\d{2}", saved["date"])
    assert saved["measured"]["vendor_comet"] == 0.87
    assert saved["gate_config"]["vendor_comet"] == 0.87 and saved["gate_config"]["chrf_min"] == 0.9
    versions = saved["versions"]
    assert versions["files"] == {n: sha(d) for n, d in FILES.items()}
    assert versions["bergamot_sha256"] == pe.sha256_file(fake)
    assert versions["python"] == platform.python_version() and versions["pin"] == "fake 1"
    assert set(versions["corpus"]) == {"translate_en_fr.jsonl"}
    target = repo / "translation-snap/plugins/translate-en-fr.measured.json"
    measured = json.loads(target.read_text(encoding="utf-8"))
    assert measured["gate"] == "fail" and measured["results_sha256"] == pe.sha256_file(out)
    assert measured["quality_source"].endswith("vendor FLORES COMET 0.87")
    assert results["measured_file"] == str(target)
    printed = capsys.readouterr().out
    assert "system: socket:fake-en-fr" in printed and f"results: {out}" in printed
    assert "gate: fail  spot check: missing  rss: " in printed
    pe.run(args_for(tmp_path, fake))  # the results directory may already exist


def test_the_binary_comes_from_the_environment_when_none_is_given(env, tmp_path, monkeypatch):
    _, fake = env
    monkeypatch.setenv("MYNA_BERGAMOT", str(fake))
    results = pe.run(args_for(tmp_path, fake, bergamot=None))
    saved = json.loads(Path(results["results_file"]).read_text(encoding="utf-8"))
    assert saved["versions"]["bergamot_sha256"] == pe.sha256_file(fake)


def test_the_host_guard_defaults_to_chug(env, tmp_path, monkeypatch):
    _, fake = env
    monkeypatch.delenv("MYNA_PIPELINE_HOST")
    monkeypatch.setattr(pe.socket, "gethostname", lambda: "elsewhere")
    with pytest.raises(pe.PipelineError, match=r"chug only .*elsewhere.*chug\.sh"):
        pe.run(args_for(tmp_path, fake))
    monkeypatch.setattr(pe.socket, "gethostname", lambda: "chug")
    with pytest.raises(GateError, match="no plugin 'nope'"):
        pe.run(args_for(tmp_path, fake, plugin="nope"))


def test_no_measured_leaves_the_plugin_directory_alone(env, tmp_path):
    repo, fake = env
    pe.run(args_for(tmp_path, fake, no_measured=True))
    assert not (repo / "translation-snap/plugins/translate-en-fr.measured.json").exists()


def test_the_command_exits_one_on_a_failed_gate_and_two_on_a_refusal(
    env, tmp_path, capsys, monkeypatch
):
    _, fake = env
    with pytest.raises(SystemExit) as raised:
        pe.cmd_plugin_eval(args_for(tmp_path, fake))
    assert raised.value.code == 1
    with pytest.raises(SystemExit) as raised:
        pe.cmd_plugin_eval(args_for(tmp_path, fake, plugin="nope"))
    assert raised.value.code == 2 and "no plugin 'nope'" in capsys.readouterr().err
    monkeypatch.setattr(pe, "run", lambda args: {"gate": "pass"})
    pe.cmd_plugin_eval(args_for(tmp_path, fake))


def test_the_pipeline_refuses_to_run_off_chug(env, tmp_path, monkeypatch):
    _, fake = env
    monkeypatch.setenv("MYNA_PIPELINE_HOST", "not-this-host")
    with pytest.raises(pe.PipelineError, match="chug only"):
        pe.run(args_for(tmp_path, fake))


def test_a_missing_bergamot_binary_is_named(env, tmp_path):
    _, fake = env
    with pytest.raises(pe.PipelineError, match="myna-bergamot not found"):
        pe.run(args_for(tmp_path, fake, bergamot=str(tmp_path / "absent")))


def test_a_daemon_that_dies_is_reported_with_its_exit_code(tmp_path, monkeypatch):
    import subprocess

    real = subprocess.Popen
    monkeypatch.setattr(
        pe.subprocess,
        "Popen",
        lambda cmd, **kw: real(
            [sys.executable, "-c", "import sys; sys.stderr.write('boom'); sys.exit(3)"], **kw
        ),
    )
    with pytest.raises(pe.PipelineError, match="exited 3: boom"):
        pe.start_daemon({"snap": "myna-text"}, tmp_path, tmp_path / "s.sock", None)


def test_a_translation_daemon_needs_a_bergamot_binary(tmp_path):
    with pytest.raises(pe.PipelineError, match="myna-bergamot not found"):
        pe.start_daemon({"snap": "myna-translation"}, tmp_path, tmp_path / "s.sock", None)


def test_bergamot_is_found_from_the_environment_or_the_path(monkeypatch, tmp_path):
    binary = tmp_path / "myna-bergamot"
    binary.write_text("")
    monkeypatch.setenv("MYNA_BERGAMOT", str(binary))
    assert pe._find_bergamot() == binary
    monkeypatch.delenv("MYNA_BERGAMOT")
    monkeypatch.setenv("PATH", str(tmp_path))
    binary.chmod(0o755)
    assert pe._find_bergamot() == binary


def test_the_results_directory_defaults_under_the_workstream_dir(monkeypatch):
    monkeypatch.delenv("MYNA_RESULTS_DIR", raising=False)
    assert pe.results_dir_default() == Path("~/myna-processing-graphs/results").expanduser()
    monkeypatch.setenv("MYNA_RESULTS_DIR", "/x/y")
    assert pe.results_dir_default() == Path("/x/y")


def test_the_arguments_default_to_the_shared_model_cache(monkeypatch):
    monkeypatch.delenv("MYNA_MODEL_CACHE", raising=False)
    parser = argparse.ArgumentParser()
    pe.add_arguments(parser)
    args = parser.parse_args(["p"])
    assert (args.plugin, args.cache, args.repeats, args.no_measured) == (
        "p",
        "~/.cache/myna/models",
        3,
        False,
    )
