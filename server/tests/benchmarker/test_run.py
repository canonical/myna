"""Snap sweep runner.

`run` is the subcommand that installs and purges snaps as root on a tester's
machine, so the parts worth pinning offline are the ones that decide *what*
gets run: config validation, target defaults, the label that every result row
is grouped by, and the guards that refuse before touching the system. The snap
commands themselves are recorded through a fake `subprocess.run` rather than
executed.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from pathlib import Path

import pytest
from _records import record

from myna.benchmarker import _run
from myna.benchmarker._run import (
    DEFAULT_SWEEP_BUDGET_S,
    ResourceSampler,
    SnapTarget,
    _chown_to_invoker,
    _gpu_memory_by_pid,
    _JsonlWriter,
    _resolve_user_home,
    _sweep_one,
    cmd_run,
    wait_for_socket,
)


class FakeRun:
    """Records every subprocess.run call and replays scripted results.

    Results are keyed by a command fragment (first match wins) rather than by
    the argv head: `use-engine --auto` and the `use-engine <name>` fallback
    share a head but must answer differently.
    """

    def __init__(self):
        self.calls: list[list[str]] = []
        self.results: list[tuple[str, int, str]] = []

    def reply(self, fragment: str, rc: int = 0, stdout: str = "") -> None:
        self.results.append((fragment, rc, stdout))

    def __call__(self, cmd, **kwargs):
        self.calls.append(list(cmd))
        joined = " ".join(str(c) for c in cmd)
        rc, stdout = 0, ""
        for fragment, frag_rc, frag_out in self.results:
            if fragment in joined:
                rc, stdout = frag_rc, frag_out
                break
        if kwargs.get("check") and rc != 0:
            raise subprocess.CalledProcessError(rc, cmd)
        return subprocess.CompletedProcess(cmd, rc, stdout=stdout, stderr="")

    def ran(self, *fragments) -> bool:
        return any(all(f in cmd for f in fragments) for cmd in self.calls)


@pytest.fixture
def fake_run(monkeypatch):
    fake = FakeRun()
    monkeypatch.setattr(_run.subprocess, "run", fake)
    return fake


# ─── wait_for_socket ─────────────────────────────────────────────────────────


def test_wait_for_socket_returns_true_once_the_path_appears(tmp_path):
    sock = tmp_path / "ubustt.sock"
    sock.write_bytes(b"")
    assert wait_for_socket(sock, timeout=1.0) is True


def test_wait_for_socket_times_out_on_a_socket_that_never_binds(tmp_path):
    started = time.monotonic()
    assert wait_for_socket(tmp_path / "never", timeout=0.5) is False
    assert time.monotonic() - started >= 0.5


def test_wait_for_socket_returns_as_soon_as_a_late_socket_binds(tmp_path):
    sock = tmp_path / "late.sock"
    threading.Timer(0.3, lambda: sock.write_bytes(b"")).start()
    started = time.monotonic()
    assert wait_for_socket(sock, timeout=10.0) is True
    assert time.monotonic() - started < 5.0


# ─── _gpu_memory_by_pid ──────────────────────────────────────────────────────


def test_gpu_memory_maps_pids_to_mib(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0, stdout="123, 512\n456, 1024\n"),
    )
    assert _gpu_memory_by_pid() == {123: 512, 456: 1024}


def test_gpu_memory_is_empty_without_nvidia_smi(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess, "run", lambda cmd, **kw: (_ for _ in ()).throw(FileNotFoundError())
    )
    assert _gpu_memory_by_pid() == {}


def test_gpu_memory_skips_unparseable_rows_rather_than_failing(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0, stdout="oops\n123, 512\n"),
    )
    assert _gpu_memory_by_pid() == {123: 512}


# ─── ResourceSampler ─────────────────────────────────────────────────────────


def test_sampler_records_a_peak_for_a_live_process(monkeypatch):
    monkeypatch.setattr(_run, "_gpu_memory_by_pid", dict)
    sampler = ResourceSampler(os.getpid(), interval=0.01)
    sampler.start()
    time.sleep(0.1)
    sampler.stop()
    assert sampler.peak_rss_mb > 0
    assert sampler.peak_vram_mb is None
    assert not sampler.is_alive()


def test_sampler_attributes_vram_only_to_pids_in_its_own_tree(monkeypatch):
    monkeypatch.setattr(_run, "_gpu_memory_by_pid", lambda: {os.getpid(): 700, 999999: 4096})
    sampler = ResourceSampler(os.getpid(), interval=0.01)
    sampler.start()
    time.sleep(0.1)
    sampler.stop()
    assert sampler.peak_vram_mb == 700.0


def test_sampler_on_a_dead_pid_stops_cleanly_at_zero(monkeypatch):
    monkeypatch.setattr(_run, "_gpu_memory_by_pid", dict)
    sampler = ResourceSampler(999999, interval=0.01)
    sampler.start()
    time.sleep(0.05)
    sampler.stop()
    assert sampler.peak_rss_mb == 0.0


# ─── invoker identity under sudo ─────────────────────────────────────────────


def test_results_are_chowned_back_to_the_invoking_user(tmp_path, monkeypatch):
    target = tmp_path / "results.jsonl"
    target.write_text("{}\n", encoding="utf-8")
    monkeypatch.setenv("SUDO_UID", "1000")
    monkeypatch.setenv("SUDO_GID", "1000")
    seen = []
    monkeypatch.setattr(_run.os, "chown", lambda p, u, g: seen.append((Path(p).name, u, g)))

    _chown_to_invoker(target)

    assert seen == [("results.jsonl", 1000, 1000)]


def test_chown_is_skipped_when_not_running_under_sudo(tmp_path, monkeypatch):
    target = tmp_path / "results.jsonl"
    target.write_text("{}\n", encoding="utf-8")
    monkeypatch.delenv("SUDO_UID", raising=False)
    monkeypatch.delenv("SUDO_GID", raising=False)
    monkeypatch.setattr(_run.os, "chown", lambda *a: pytest.fail("chowned without SUDO_UID"))

    _chown_to_invoker(target)


def test_chown_is_skipped_for_a_file_that_was_never_written(tmp_path, monkeypatch):
    monkeypatch.setenv("SUDO_UID", "1000")
    monkeypatch.setenv("SUDO_GID", "1000")
    monkeypatch.setattr(_run.os, "chown", lambda *a: pytest.fail("chowned a missing path"))

    _chown_to_invoker(tmp_path / "absent.jsonl")


def test_home_is_repointed_at_the_invoking_user(monkeypatch):
    import pwd

    monkeypatch.setenv("SUDO_UID", "1000")
    monkeypatch.setenv("HOME", "/root")
    monkeypatch.setattr(
        pwd, "getpwuid", lambda uid: type("Ent", (), {"pw_dir": f"/home/user{uid}"})()
    )

    _resolve_user_home()

    assert os.environ["HOME"] == "/home/user1000"


def test_home_is_left_alone_without_sudo(monkeypatch):
    monkeypatch.delenv("SUDO_UID", raising=False)
    monkeypatch.setenv("HOME", "/root")
    _resolve_user_home()
    assert os.environ["HOME"] == "/root"


def test_home_is_left_alone_when_the_invoking_uid_has_no_passwd_entry(monkeypatch):
    import pwd

    monkeypatch.setenv("SUDO_UID", "4242")
    monkeypatch.setenv("HOME", "/root")
    monkeypatch.setattr(pwd, "getpwuid", lambda uid: (_ for _ in ()).throw(KeyError(uid)))

    _resolve_user_home()

    assert os.environ["HOME"] == "/root"


# ─── SnapTarget ──────────────────────────────────────────────────────────────


def test_target_defaults_cli_service_and_socket_from_the_snap_name(tmp_path):
    target = SnapTarget({"snap": "whisper", "files": [str(tmp_path / "whisper.snap")]})
    assert target.cli == "whisper"
    assert target.service == "whisper.server"
    assert target.socket == Path("/var/snap/whisper/common/run/ubustt.sock")


def test_target_overrides_win_over_the_defaults(tmp_path):
    target = SnapTarget(
        {
            "snap": "whisper",
            "files": [str(tmp_path / "whisper.snap")],
            "cli": "whisper.ctl",
            "service": "whisper.daemon",
            "socket": "/tmp/custom.sock",
        }
    )
    assert (target.cli, target.service) == ("whisper.ctl", "whisper.daemon")
    assert target.socket == Path("/tmp/custom.sock")


def test_target_resolves_file_paths_at_parse_time(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "whisper.snap").write_bytes(b"")
    target = SnapTarget({"snap": "whisper", "files": ["./whisper.snap"]})
    assert target.files == [str(tmp_path / "whisper.snap")]


def test_a_target_with_no_files_is_rejected_by_name():
    with pytest.raises(SystemExit, match="'whisper': no files listed"):
        SnapTarget({"snap": "whisper", "files": []})


def test_a_target_with_a_missing_files_key_is_rejected():
    with pytest.raises(SystemExit, match="no files listed"):
        SnapTarget({"snap": "whisper"})


def target_for(**spec):
    return SnapTarget({"snap": "whisper", "files": ["/tmp/whisper.snap"], **spec})


def test_label_is_snap_engine_model_mode():
    target = target_for()
    target.engine, target.model = "cpu", "tiny"
    assert target.label == "whisper/cpu/tiny/batch"


def test_label_names_the_engine_as_unknown_before_describe():
    assert target_for().label == "whisper/unknown-engine/batch"


def test_label_omits_the_model_when_the_snap_reports_none():
    target = target_for()
    target.engine = "cpu"
    assert target.label == "whisper/cpu/batch"


def test_label_distinguishes_streaming_from_batch():
    target = target_for()
    target.engine, target.model, target.streaming = "cpu", "tiny", True
    assert target.label == "whisper/cpu/tiny/streaming"


def test_label_carries_the_streaming_variant_suffix():
    target = target_for()
    target.engine, target.model = "cpu", "tiny"
    target.streaming, target.config_suffix = True, "arm3s"
    assert target.label == "whisper/cpu/tiny/streaming-arm3s"


def test_a_batch_label_ignores_a_stale_variant_suffix():
    target = target_for()
    target.engine, target.config_suffix = "cpu", "arm3s"
    assert target.label.endswith("/batch")


def test_purge_removes_the_snap(fake_run):
    target_for().purge()
    assert fake_run.ran("snap", "remove", "--purge", "whisper")


def test_models_are_read_from_the_snap_cli(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(
            cmd, 0, stdout=json.dumps({"models": [{"name": "tiny"}, {"name": "base"}]})
        ),
    )
    assert target_for().models() == ["tiny", "base"]


def test_models_are_filtered_to_the_configured_allowlist(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(
            cmd, 0, stdout=json.dumps({"models": [{"name": "tiny"}, {"name": "base"}]})
        ),
    )
    assert target_for(models=["base"]).models() == ["base"]


def test_an_allowlisted_model_the_engine_does_not_offer_is_a_config_error(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(
            cmd, 0, stdout=json.dumps({"models": [{"name": "tiny"}]})
        ),
    )
    with pytest.raises(SystemExit, match=r"not offered by .*: \['huge'\]"):
        target_for(models=["huge"]).models()


def test_models_is_empty_when_the_cli_cannot_answer(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess, "run", lambda cmd, **kw: (_ for _ in ()).throw(FileNotFoundError())
    )
    assert target_for().models() == []


def test_models_is_empty_when_the_cli_returns_junk(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0, stdout="{{")
    )
    assert target_for().models() == []


@pytest.mark.parametrize(("rc", "expected"), [(0, True), (1, False)])
def test_streaming_support_is_probed_with_a_config_get(monkeypatch, rc, expected):
    monkeypatch.setattr(
        _run.subprocess, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, rc)
    )
    assert target_for().supports_streaming() is expected


def test_pid_comes_from_the_service_unit(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0, stdout="4321\n"),
    )
    assert target_for().pid == 4321


@pytest.mark.parametrize("stdout", ["0\n", "\n", "not-a-pid\n"])
def test_pid_is_none_when_the_unit_is_not_running(monkeypatch, stdout):
    monkeypatch.setattr(
        _run.subprocess,
        "run",
        lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0, stdout=stdout),
    )
    assert target_for().pid is None


def test_pid_is_none_when_systemctl_is_absent(monkeypatch):
    monkeypatch.setattr(
        _run.subprocess, "run", lambda cmd, **kw: (_ for _ in ()).throw(FileNotFoundError())
    )
    assert target_for().pid is None


def test_unconnected_plugs_are_connected_and_connected_ones_left_alone(fake_run):
    fake_run.reply(
        "snap connections",
        stdout=(
            "Interface  Plug              Slot     Notes\n"
            "audio      whisper:audio     :audio   -\n"
            "hardware   whisper:hw        -        -\n"
        ),
    )
    target_for()._connect_plugs()
    assert fake_run.ran("snap", "connect", "whisper:hw")
    assert not fake_run.ran("snap", "connect", "whisper:audio")


def test_engine_selection_prefers_auto_detection(fake_run):
    target_for()._select_engine()
    assert fake_run.ran("use-engine", "--auto")
    assert not fake_run.ran("list-engines", "--format=json")


def test_engine_selection_falls_back_to_the_first_listed_engine(fake_run):
    fake_run.reply("use-engine --auto", rc=1)
    fake_run.reply("list-engines", stdout=json.dumps({"engines": [{"name": "cpu"}]}))
    target_for()._select_engine()
    assert fake_run.ran("use-engine", "cpu", "--assume-yes")


def test_engine_selection_gives_up_quietly_when_nothing_is_listed(fake_run, capsys):
    fake_run.reply("use-engine --auto", rc=1)
    fake_run.reply("list-engines", stdout=json.dumps({"engines": []}))
    target_for()._select_engine()
    assert "engine selection skipped" in capsys.readouterr().out


def test_start_fails_loudly_when_the_socket_never_appears(fake_run, monkeypatch, tmp_path):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: False)
    with pytest.raises(SystemExit, match="did not appear"):
        target_for(socket=str(tmp_path / "never.sock")).start()


def test_start_installs_connects_and_describes(fake_run, monkeypatch, capsys):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: True)
    fake_run.reply("show-engine", stdout=json.dumps({"name": "cpu"}))

    target = target_for()
    target.start()

    assert fake_run.ran("snap", "install", "--dangerous")
    assert fake_run.ran("snap", "start", "whisper.server")
    assert target.engine == "cpu"
    assert "serving engine=cpu" in capsys.readouterr().out


def test_switching_to_streaming_clears_a_previous_variant_suffix(fake_run, monkeypatch):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: True)
    target = target_for()
    target.config_suffix = "arm3s"

    target.set_streaming(True)

    assert target.streaming is True
    assert target.config_suffix == ""
    assert fake_run.ran("set", "--assume-yes", "--no-restart", "streaming=true")


def test_a_streaming_variant_passes_its_settings_through(fake_run, monkeypatch):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: True)
    target = target_for()

    target.set_streaming_variant({"stream-arm-seconds": "3"}, "arm3s")

    assert target.streaming is True and target.config_suffix == "arm3s"
    assert fake_run.ran("set", "streaming=true", "stream-arm-seconds=3")


def test_a_model_switch_that_loses_the_socket_is_fatal(fake_run, monkeypatch):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: False)
    with pytest.raises(SystemExit, match="did not return after switching to tiny"):
        target_for().use_model("tiny")


@pytest.mark.parametrize(
    ("method", "args", "message"),
    [
        ("set_streaming", (True,), "switching to streaming"),
        ("set_streaming_variant", ({}, "arm3s"), "switching to streaming-arm3s"),
    ],
)
def test_a_mode_switch_that_loses_the_socket_is_fatal(fake_run, monkeypatch, method, args, message):
    monkeypatch.setattr(_run, "_run", fake_run)
    monkeypatch.setattr(_run, "wait_for_socket", lambda path, **kw: False)
    with pytest.raises(SystemExit, match=message):
        getattr(target_for(), method)(*args)


# ─── _JsonlWriter ────────────────────────────────────────────────────────────


def test_writer_appends_one_json_object_per_line(tmp_path):
    path = tmp_path / "out.jsonl"
    with _JsonlWriter(path) as out:
        out.write({"a": 1})
        out.write({"b": 2})
    assert [json.loads(ln) for ln in path.read_text(encoding="utf-8").splitlines()] == [
        {"a": 1},
        {"b": 2},
    ]


def test_writer_flushes_each_record_so_a_killed_sweep_keeps_its_results(tmp_path):
    path = tmp_path / "out.jsonl"
    with _JsonlWriter(path) as out:
        out.write({"a": 1})
        assert path.read_text(encoding="utf-8") == '{"a": 1}\n'


def test_writer_appends_to_an_existing_file(tmp_path):
    path = tmp_path / "out.jsonl"
    path.write_text('{"existing": true}\n', encoding="utf-8")
    with _JsonlWriter(path) as out:
        out.write({"new": True})
    assert len(path.read_text(encoding="utf-8").splitlines()) == 2


# ─── _sweep_one ──────────────────────────────────────────────────────────────


class FakeClip:
    """Just enough Clip for the sweep's progress printing."""

    def __init__(self, clip_id="clip-a"):
        self.id = clip_id


class FakeTarget:
    def __init__(self, pid=None, streaming=False):
        self.snap = "whisper"
        self.socket = Path("/tmp/whisper.sock")
        self.streaming = streaming
        self.pid = pid
        self.label = "whisper/cpu/tiny/batch"


def stub_run_clips(monkeypatch, *results):
    """Queue (overran, scored) results for successive run_clips calls."""
    queue = list(results)
    calls = []

    async def run_clips(**kwargs):
        calls.append(kwargs)
        return queue.pop(0)

    module = type("M", (), {"run_clips": staticmethod(run_clips)})
    monkeypatch.setitem(__import__("sys").modules, "myna.benchmarker._bench", module)
    return calls


def sweep(tmp_path, target, **overrides):
    kwargs = {
        "target": target,
        "clips_cold": [],
        "clips_warm": [FakeClip("clip-warm")],
        "budget": 60.0,
        "out": _JsonlWriter(tmp_path / "out.jsonl"),
        "provenance": {"machine": "box"},
        "resources_path": tmp_path / "resources.jsonl",
        "sample_resources": False,
        "broken": [],
        "unusable": [],
    }
    kwargs.update(overrides)
    _sweep_one(**kwargs)
    return kwargs


def test_a_clean_sweep_records_no_failures(tmp_path, monkeypatch):
    stub_run_clips(monkeypatch, (False, 1))
    kwargs = sweep(tmp_path, FakeTarget())
    assert kwargs["broken"] == [] and kwargs["unusable"] == []


def test_a_cold_sample_runs_before_the_warm_sweep(tmp_path, monkeypatch):
    calls = stub_run_clips(monkeypatch, (False, 1), (False, 1))
    sweep(tmp_path, FakeTarget(), clips_cold=[FakeClip("clip-cold")])
    assert [c["cold"] for c in calls] == [True, False]


def test_a_cold_sample_that_overruns_abandons_the_target(tmp_path, monkeypatch):
    calls = stub_run_clips(monkeypatch, (True, 0))
    kwargs = sweep(tmp_path, FakeTarget(), clips_cold=[FakeClip("clip-cold")])
    assert len(calls) == 1  # the warm sweep never started
    assert kwargs["unusable"] == [("whisper/cpu/tiny/batch", "cold sample overran")]


def test_a_warm_sweep_over_budget_is_recorded_as_unusable(tmp_path, monkeypatch):
    stub_run_clips(monkeypatch, (True, 3))
    kwargs = sweep(tmp_path, FakeTarget())
    assert kwargs["unusable"] == [("whisper/cpu/tiny/batch", "exceeded 60s budget")]


def test_the_warm_sweep_budget_is_recorded_in_provenance(tmp_path, monkeypatch):
    calls = stub_run_clips(monkeypatch, (False, 1))
    sweep(tmp_path, FakeTarget(), budget=42.0)
    assert calls[0]["provenance"]["sweep_budget_seconds"] == 42.0


def test_a_crashing_target_is_recorded_and_does_not_propagate(tmp_path, monkeypatch, capsys):
    async def boom(**kwargs):
        raise RuntimeError("adapter died")

    module = type("M", (), {"run_clips": staticmethod(boom)})
    monkeypatch.setitem(__import__("sys").modules, "myna.benchmarker._bench", module)

    kwargs = sweep(tmp_path, FakeTarget())

    assert kwargs["broken"] == [("whisper/cpu/tiny/batch", "RuntimeError: adapter died")]
    assert "FAILED: RuntimeError" in capsys.readouterr().out


def test_a_nonzero_subprocess_exit_is_recorded_by_return_code(tmp_path, monkeypatch):
    async def boom(**kwargs):
        raise subprocess.CalledProcessError(3, ["snap"])

    module = type("M", (), {"run_clips": staticmethod(boom)})
    monkeypatch.setitem(__import__("sys").modules, "myna.benchmarker._bench", module)

    kwargs = sweep(tmp_path, FakeTarget())

    assert kwargs["broken"] == [("whisper/cpu/tiny/batch", "exited 3")]


def test_resource_peaks_are_written_to_the_sidecar_when_sampling(tmp_path, monkeypatch):
    stub_run_clips(monkeypatch, (False, 1))
    monkeypatch.setattr(_run, "_gpu_memory_by_pid", dict)
    resources = tmp_path / "resources.jsonl"

    sweep(tmp_path, FakeTarget(pid=os.getpid()), sample_resources=True, resources_path=resources)

    peak = json.loads(resources.read_text(encoding="utf-8").strip())
    assert peak["label"] == "whisper/cpu/tiny/batch"
    assert peak["snap"] == "whisper"
    assert peak["peak_rss_mb"] > 0
    assert peak["peak_vram_mb"] is None


def test_peaks_are_written_even_when_the_target_crashed(tmp_path, monkeypatch):
    async def boom(**kwargs):
        raise RuntimeError("adapter died")

    module = type("M", (), {"run_clips": staticmethod(boom)})
    monkeypatch.setitem(__import__("sys").modules, "myna.benchmarker._bench", module)
    monkeypatch.setattr(_run, "_gpu_memory_by_pid", dict)
    resources = tmp_path / "resources.jsonl"

    sweep(tmp_path, FakeTarget(pid=os.getpid()), sample_resources=True, resources_path=resources)

    assert resources.exists()


def test_no_sidecar_is_written_when_the_service_has_no_pid(tmp_path, monkeypatch):
    stub_run_clips(monkeypatch, (False, 1))
    resources = tmp_path / "resources.jsonl"
    sweep(tmp_path, FakeTarget(pid=None), sample_resources=True, resources_path=resources)
    assert not resources.exists()


# ─── cmd_run guards ──────────────────────────────────────────────────────────


class RunArgs:
    def __init__(self, config, out=None, keep_results=False, no_resources=True, budget=None):
        self.config = str(config)
        self.out = str(out) if out else None
        self.keep_results = keep_results
        self.no_resources = no_resources
        self.budget = budget


def write_config(path, **overrides):
    cfg = {
        "manifest": "corpus/manifest.json",
        "out": "results.jsonl",
        "targets": [{"snap": "whisper", "files": ["/tmp/whisper.snap"]}],
    }
    cfg.update(overrides)
    path.write_text(json.dumps(cfg), encoding="utf-8")  # JSON is valid YAML
    return path


def test_a_missing_config_names_the_path_and_the_help_command(tmp_path):
    with pytest.raises(SystemExit, match="config not found"):
        cmd_run(RunArgs(tmp_path / "absent.yaml"))


def test_a_config_with_no_targets_is_rejected(tmp_path):
    config = write_config(tmp_path / "bench.yaml", targets=[])
    with pytest.raises(SystemExit, match="no targets in config"):
        cmd_run(RunArgs(config))


def test_a_missing_manifest_points_at_download_corpus(tmp_path):
    config = write_config(tmp_path / "bench.yaml", manifest=str(tmp_path / "absent.json"))
    with pytest.raises(SystemExit, match="download-corpus"):
        cmd_run(RunArgs(config))


def test_run_refuses_without_root_before_touching_any_snap(tmp_path, monkeypatch):
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"schema_version": 1, "clips": []}), encoding="utf-8")
    config = write_config(tmp_path / "bench.yaml", manifest=str(manifest))
    monkeypatch.setattr(_run.os, "geteuid", lambda: 1000)
    monkeypatch.setattr(
        _run.subprocess, "run", lambda *a, **kw: pytest.fail("ran a snap command as non-root")
    )

    with pytest.raises(SystemExit, match="requires root"):
        cmd_run(RunArgs(config))


def test_the_default_sweep_budget_applies_when_the_config_omits_one():
    assert DEFAULT_SWEEP_BUDGET_S == 600.0


# ─── cmd_run happy path ──────────────────────────────────────────────────────


@pytest.fixture
def corpus(tmp_path):
    """A two-clip manifest whose WAVs exist on disk."""
    import wave

    audio = tmp_path / "audio"
    audio.mkdir()
    clips = []
    for clip_id in ("clip-a", "clip-b"):
        path = audio / f"{clip_id}.wav"
        with wave.open(str(path), "w") as wf:
            wf.setnchannels(1)
            wf.setsampwidth(2)
            wf.setframerate(16_000)
            wf.writeframes(b"\x00\x00" * 1600)
        clips.append(
            {
                "id": clip_id,
                "path": f"audio/{clip_id}.wav",
                "text": "hello world",
                "language": "en",
                "category": "quiet",
                "duration_seconds": 0.1,
                "sample_rate_hz": 16_000,
                "channels": 1,
                "source": "test",
                "license": "CC0-1.0",
            }
        )
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"schema_version": 1, "clips": clips}), encoding="utf-8")
    return manifest


@pytest.fixture
def stub_sweep(monkeypatch):
    """Replace the per-target sweep with a recorder that writes one record."""
    swept = []

    def fake(*, target, out, **kwargs):
        swept.append((target.label, kwargs["clips_cold"], kwargs["clips_warm"]))
        out.write(record(label=target.label))

    monkeypatch.setattr(_run, "_sweep_one", fake)
    return swept


@pytest.fixture
def stub_target(monkeypatch):
    """A SnapTarget that never touches snapd."""

    class Stub(SnapTarget):
        started = 0
        stopped = 0

        def start(self):
            type(self).started += 1
            self.engine, self.model = "cpu", "tiny"

        def stop(self):
            type(self).stopped += 1

        def models(self):
            return ["tiny"]

        def supports_streaming(self):
            return False

        def set_streaming(self, streaming):
            self.streaming = streaming

        def set_streaming_variant(self, settings, suffix):
            self.streaming, self.config_suffix = True, suffix

        def use_model(self, model):
            self.model = model

    Stub.started = Stub.stopped = 0
    monkeypatch.setattr(_run, "SnapTarget", Stub)
    return Stub


@pytest.fixture
def rootless(monkeypatch):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    monkeypatch.setattr(
        _run.machine if hasattr(_run, "machine") else _run,
        "__name__",
        _run.__name__,
    )


def test_a_sweep_writes_the_machine_header_then_one_record_per_target(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch, capsys
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out))

    lines = [json.loads(ln) for ln in out.read_text(encoding="utf-8").splitlines()]
    assert lines[0]["type"] == "machine"
    assert lines[1]["label"] == "whisper/cpu/tiny/batch"
    assert stub_target.started == 1 and stub_target.stopped == 1
    assert "RESULTS" in capsys.readouterr().out


def test_the_cold_clip_is_held_out_of_the_warm_sweep(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    config = write_config(
        tmp_path / "bench.yaml", manifest=str(corpus), out=str(out), cold_clip="clip-a"
    )

    cmd_run(RunArgs(config, out=out))

    (_, cold, warm) = stub_sweep[0]
    assert [c.id for c in cold] == ["clip-a"]
    assert [c.id for c in warm] == ["clip-b"]


def test_a_cold_clip_absent_from_the_manifest_is_a_config_error(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    config = write_config(
        tmp_path / "bench.yaml",
        manifest=str(corpus),
        out=str(tmp_path / "r.jsonl"),
        cold_clip="nope",
    )
    with pytest.raises(SystemExit, match="cold_clip 'nope' not in manifest"):
        cmd_run(RunArgs(config))


def test_an_explicit_clip_list_is_honoured(tmp_path, corpus, stub_sweep, stub_target, monkeypatch):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    config = write_config(
        tmp_path / "bench.yaml", manifest=str(corpus), out=str(out), clips=["clip-b"]
    )

    cmd_run(RunArgs(config, out=out))

    assert [c.id for c in stub_sweep[0][2]] == ["clip-b"]


def test_clips_absent_from_the_manifest_are_a_config_error(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    config = write_config(
        tmp_path / "bench.yaml",
        manifest=str(corpus),
        out=str(tmp_path / "r.jsonl"),
        clips=["clip-a", "ghost"],
    )
    with pytest.raises(SystemExit, match=r"clips not in manifest: \['ghost'\]"):
        cmd_run(RunArgs(config))


def test_a_rerun_resets_the_results_file_by_default(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch, capsys
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    out.write_text('{"stale": true}\n', encoding="utf-8")
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out))

    assert "stale" not in out.read_text(encoding="utf-8")
    assert "resetting" in capsys.readouterr().out


def test_keep_results_appends_to_the_existing_file(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    out.write_text(json.dumps(record(label="previous/run")) + "\n", encoding="utf-8")
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out, keep_results=True))

    assert "previous/run" in out.read_text(encoding="utf-8")


def test_a_target_that_fails_to_start_is_reported_and_the_sweep_continues(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch, capsys
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)

    def explode(self):
        raise RuntimeError("snapd refused")

    monkeypatch.setattr(stub_target, "start", explode)
    out = tmp_path / "results.jsonl"
    config = write_config(
        tmp_path / "bench.yaml",
        manifest=str(corpus),
        out=str(out),
        targets=[
            {"snap": "whisper", "files": ["/tmp/whisper.snap"]},
            {"snap": "parakeet", "files": ["/tmp/parakeet.snap"]},
        ],
    )

    cmd_run(RunArgs(config, out=out))

    printed = capsys.readouterr().out
    assert "2 target(s) failed" in printed
    assert "snapd refused" in printed
    assert stub_target.stopped == 2  # both targets were purged regardless


def test_streaming_variants_are_swept_one_per_config(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    monkeypatch.setattr(stub_target, "supports_streaming", lambda self: True)
    out = tmp_path / "results.jsonl"
    config = write_config(
        tmp_path / "bench.yaml",
        manifest=str(corpus),
        out=str(out),
        targets=[
            {
                "snap": "whisper",
                "files": ["/tmp/whisper.snap"],
                "streaming_configs": [
                    {"label": "arm3s", "settings": {"stream-arm-seconds": "3"}},
                    {"label": "arm5s", "settings": {"stream-arm-seconds": "5"}},
                ],
            }
        ],
    )

    cmd_run(RunArgs(config, out=out))

    labels = [label for label, _, _ in stub_sweep]
    assert labels == [
        "whisper/cpu/tiny/batch",
        "whisper/cpu/tiny/streaming-arm3s",
        "whisper/cpu/tiny/streaming-arm5s",
    ]


def test_a_togglable_snap_without_variants_sweeps_batch_then_streaming(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    monkeypatch.setattr(stub_target, "supports_streaming", lambda self: True)
    out = tmp_path / "results.jsonl"
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out))

    assert [label for label, _, _ in stub_sweep] == [
        "whisper/cpu/tiny/batch",
        "whisper/cpu/tiny/streaming",
    ]


def test_a_snap_reporting_no_models_is_still_swept_once(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    monkeypatch.setattr(stub_target, "models", lambda self: [])
    out = tmp_path / "results.jsonl"
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out))

    assert len(stub_sweep) == 1


def test_the_budget_flag_overrides_the_config(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch, capsys
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    out = tmp_path / "results.jsonl"
    config = write_config(
        tmp_path / "bench.yaml", manifest=str(corpus), out=str(out), sweep_budget_seconds=300
    )

    cmd_run(RunArgs(config, out=out, budget=45.0))

    assert "budget: 45s" in capsys.readouterr().out


def test_results_are_handed_back_to_the_invoking_user(
    tmp_path, corpus, stub_sweep, stub_target, monkeypatch
):
    monkeypatch.setattr(_run.os, "geteuid", lambda: 0)
    chowned = []
    monkeypatch.setattr(_run, "_chown_to_invoker", lambda p: chowned.append(Path(p).name))
    out = tmp_path / "results.jsonl"
    config = write_config(tmp_path / "bench.yaml", manifest=str(corpus), out=str(out))

    cmd_run(RunArgs(config, out=out))

    assert chowned == ["results.jsonl", "results-resources.jsonl"]
