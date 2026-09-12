"""The environment guard that gates a sweep.

Every check has a demonstrated failure mode - the first pass of an early
baseline was wrong by 18x because the shell ran under an 800 MB cgroup cap
against a 794 MB model, with no OOM and no warning. What is pinned here is
mostly the *scope*: which checks apply to an in-process model run and which to
a packaged daemon, because running the wrong set is how the guard would come to
be routinely skipped.
"""

from __future__ import annotations

import pytest

from myna.benchmarker import guard
from myna.benchmarker.guard import (
    HARD,
    PROFILES,
    WARN,
    Violation,
    check,
    check_page_faults,
    check_sweep_environment,
    cmd_check,
)


class CheckArgs:
    def __init__(self, model="parakeet", sweep=False, force=False, json=False):
        self.model = model
        self.sweep = sweep
        self.force = force
        self.json = json


@pytest.fixture
def clean(monkeypatch):
    """Silence every individual check, so scope is what the test observes."""
    for name in (
        "check_cgroup_memory",
        "check_cpu_governor",
        "check_core_homogeneity",
        "check_competing_service",
        "check_competing_processes",
        "check_system_load",
        "check_available_memory",
    ):
        monkeypatch.setattr(guard, name, lambda *a, **kw: [])


def fire(monkeypatch, name, severity=HARD):
    monkeypatch.setattr(
        guard,
        name,
        lambda *a, **kw: [Violation(check=name, severity=severity, message=f"{name} fired")],
    )


# ─── scope ───────────────────────────────────────────────────────────────────


def test_the_sweep_subset_ignores_the_callers_cgroup(monkeypatch, clean):
    """A snap's inference runs in snap.<snap>.<app>.service, a different cgroup
    entirely - the caller's cap says nothing about the measured process."""
    fire(monkeypatch, "check_cgroup_memory")
    assert check_sweep_environment() == []
    assert [v.check for v in check(PROFILES["whisper"])] == ["check_cgroup_memory"]


def test_the_sweep_subset_ignores_core_homogeneity(monkeypatch, clean):
    """It fails whenever affinity is unpinned, which is exactly how the daemon
    ships. Pinning the sweep would measure a configuration no user runs."""
    fire(monkeypatch, "check_core_homogeneity")
    assert check_sweep_environment() == []
    assert [v.check for v in check(PROFILES["whisper"])] == ["check_core_homogeneity"]


@pytest.mark.parametrize(
    "name", ["check_cpu_governor", "check_competing_processes", "check_system_load"]
)
def test_machine_policy_still_applies_to_a_sweep(monkeypatch, clean, name):
    fire(monkeypatch, name)
    assert [v.check for v in check_sweep_environment()] == [name]


def test_the_memory_floor_joins_the_sweep_only_with_a_profile(monkeypatch, clean):
    fire(monkeypatch, "check_available_memory", severity=WARN)
    assert check_sweep_environment() == []
    assert [v.check for v in check_sweep_environment(PROFILES["parakeet"])] == [
        "check_available_memory"
    ]


def test_every_profile_names_the_weights_its_memory_floor_was_measured_on():
    """The floor is a measurement, not an estimate, and the message has to say
    what it was measured on or nobody can tell whether it is still true."""
    for profile in PROFILES.values():
        assert profile.weights_note
        assert profile.min_cgroup_bytes > profile.peak_rss_bytes


# ─── page faults ─────────────────────────────────────────────────────────────


def test_a_quiet_measured_region_reports_nothing():
    assert check_page_faults(100, 150) is None


def test_page_fault_thrashing_is_hard_and_names_the_fix():
    violation = check_page_faults(0, 200_000)
    assert violation is not None
    assert violation.severity == HARD
    assert "memory.high" in violation.message


def test_the_page_fault_threshold_is_the_runtime_detectors():
    """Shared with myna.server.lifecycle so the dev-time guard and the runtime
    memory-pressure detector cannot drift on what 'a major fault' means."""
    from myna.server.lifecycle import MAJOR_PAGE_FAULT_THRESHOLD

    assert guard.MAX_MAJOR_PAGE_FAULTS == MAJOR_PAGE_FAULT_THRESHOLD


# ─── cmd_check ───────────────────────────────────────────────────────────────


def test_a_clean_machine_says_so_and_exits_zero(clean, capsys):
    cmd_check(CheckArgs())
    assert "environment: clean" in capsys.readouterr().out


def test_a_hard_violation_exits_nonzero(monkeypatch, clean):
    fire(monkeypatch, "check_cpu_governor")
    with pytest.raises(SystemExit):
        cmd_check(CheckArgs(sweep=True))


def test_a_warning_alone_does_not_fail_the_check(monkeypatch, clean, capsys):
    fire(monkeypatch, "check_system_load", severity=WARN)
    cmd_check(CheckArgs(sweep=True))
    assert "check_system_load fired" in capsys.readouterr().out


def test_force_records_the_number_anyway(monkeypatch, clean):
    fire(monkeypatch, "check_cpu_governor")
    cmd_check(CheckArgs(sweep=True, force=True))


def test_json_output_is_machine_readable(monkeypatch, clean, capsys):
    import json as jsonlib

    fire(monkeypatch, "check_cpu_governor")
    with pytest.raises(SystemExit):
        cmd_check(CheckArgs(sweep=True, json=True))
    payload = jsonlib.loads(capsys.readouterr().out)
    assert payload[0]["check"] == "check_cpu_governor"
    assert payload[0]["severity"] == HARD


def test_an_unknown_profile_names_the_ones_that_exist(clean):
    with pytest.raises(SystemExit, match="unknown profile"):
        cmd_check(CheckArgs(model="llama"))
