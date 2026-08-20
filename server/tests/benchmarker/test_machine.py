"""Machine-summary header record.

Every submitted results file leads with this record, so recipients can group
and filter by hardware without trusting hand-annotated provenance. The whole
point is that it degrades to None rather than raising: a probe that throws on
a container, a non-Ubuntu host, or a machine with no nvidia-smi would take the
whole sweep down with it.
"""

from __future__ import annotations

import subprocess

import pytest

from myna.benchmarker import machine

CPUINFO = """\
processor\t: 0
model name\t: AMD Ryzen 7 7840U
processor\t: 1
model name\t: AMD Ryzen 7 7840U
"""

OS_RELEASE = 'NAME="Ubuntu"\nPRETTY_NAME="Ubuntu 24.04.1 LTS"\nVERSION_ID="24.04"\n'


@pytest.fixture
def fake_proc_files(monkeypatch):
    """Route machine.py's Path reads to in-memory content, keyed by path."""
    contents: dict[str, str | OSError] = {}
    real_read = machine.Path.read_text

    def read_text(self, *args, **kwargs):
        entry = contents.get(str(self))
        if isinstance(entry, OSError):
            raise entry
        if entry is not None:
            return entry
        return real_read(self, *args, **kwargs)

    monkeypatch.setattr(machine.Path, "read_text", read_text)
    return contents


# ─── CPU ─────────────────────────────────────────────────────────────────────


def test_cpu_model_reads_the_first_model_name_line(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = CPUINFO
    assert machine._cpu_model() == "AMD Ryzen 7 7840U"


def test_cpu_model_is_none_when_cpuinfo_has_no_model_name(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = "processor\t: 0\n"
    assert machine._cpu_model() is None


def test_cpu_model_is_none_when_cpuinfo_is_unreadable(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = OSError("no /proc")
    assert machine._cpu_model() is None


def test_cpu_cores_counts_processor_lines(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = CPUINFO
    assert machine._cpu_cores() == 2


def test_cpu_cores_is_none_rather_than_zero_when_nothing_matches(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = "\n"
    assert machine._cpu_cores() is None


def test_cpu_cores_is_none_when_cpuinfo_is_unreadable(fake_proc_files):
    fake_proc_files["/proc/cpuinfo"] = OSError("no /proc")
    assert machine._cpu_cores() is None


# ─── RAM ─────────────────────────────────────────────────────────────────────


def test_ram_gb_is_rounded_to_one_decimal():
    assert isinstance(machine._ram_gb(), float)


def test_ram_gb_is_none_when_psutil_cannot_answer(monkeypatch):
    import psutil

    monkeypatch.setattr(
        psutil, "virtual_memory", lambda: (_ for _ in ()).throw(RuntimeError("no /proc/meminfo"))
    )
    assert machine._ram_gb() is None


# ─── GPU ─────────────────────────────────────────────────────────────────────


def fake_run(stdout="", raises=None):
    def run(cmd, **kwargs):
        if raises is not None:
            raise raises
        return subprocess.CompletedProcess(cmd, 0, stdout=stdout, stderr="")

    return run


def test_gpu_parses_name_and_converts_mib_to_gb(monkeypatch):
    monkeypatch.setattr(subprocess, "run", fake_run("NVIDIA RTX A2000, 4096\n"))
    assert machine._gpu() == ("NVIDIA RTX A2000", 4.0)


def test_gpu_is_none_when_nvidia_smi_is_absent(monkeypatch):
    monkeypatch.setattr(subprocess, "run", fake_run(raises=FileNotFoundError("nvidia-smi")))
    assert machine._gpu() == (None, None)


def test_gpu_is_none_when_nvidia_smi_reports_no_devices(monkeypatch):
    monkeypatch.setattr(subprocess, "run", fake_run(""))
    assert machine._gpu() == (None, None)


def test_gpu_is_none_when_the_output_is_unparseable(monkeypatch):
    monkeypatch.setattr(subprocess, "run", fake_run("garbage without a comma\n"))
    assert machine._gpu() == (None, None)


# ─── OS ──────────────────────────────────────────────────────────────────────


def test_ubuntu_version_reads_pretty_name_unquoted(fake_proc_files):
    fake_proc_files["/etc/os-release"] = OS_RELEASE
    assert machine._ubuntu_version() == "Ubuntu 24.04.1 LTS"


def test_ubuntu_version_is_none_without_a_pretty_name(fake_proc_files):
    fake_proc_files["/etc/os-release"] = 'NAME="Alpine"\n'
    assert machine._ubuntu_version() is None


def test_ubuntu_version_is_none_when_os_release_is_missing(fake_proc_files):
    fake_proc_files["/etc/os-release"] = OSError("no /etc/os-release")
    assert machine._ubuntu_version() is None


# ─── collect ─────────────────────────────────────────────────────────────────


def test_collect_emits_the_header_record_schema(fake_proc_files, monkeypatch):
    fake_proc_files["/proc/cpuinfo"] = CPUINFO
    fake_proc_files["/etc/os-release"] = OS_RELEASE
    monkeypatch.setattr(subprocess, "run", fake_run("NVIDIA RTX A2000, 4096\n"))

    header = machine.collect()

    assert header["type"] == "machine"
    assert header["cpu"] == "AMD Ryzen 7 7840U"
    assert header["cpu_cores"] == 2
    assert header["gpu"] == "NVIDIA RTX A2000"
    assert header["gpu_vram_gb"] == 4.0
    assert header["ubuntu"] == "Ubuntu 24.04.1 LTS"
    assert header["hostname"] and header["kernel"]
    assert header["collected_at"].endswith("+00:00")


def test_collect_survives_a_host_where_every_probe_fails(fake_proc_files, monkeypatch):
    fake_proc_files["/proc/cpuinfo"] = OSError("no /proc")
    fake_proc_files["/etc/os-release"] = OSError("no /etc/os-release")
    monkeypatch.setattr(subprocess, "run", fake_run(raises=FileNotFoundError("nvidia-smi")))
    monkeypatch.setattr(machine, "_ram_gb", lambda: None)

    header = machine.collect()

    assert header["type"] == "machine"
    assert [header[k] for k in ("cpu", "cpu_cores", "ram_gb", "gpu", "ubuntu")] == [None] * 5
