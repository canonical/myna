"""Collect machine summary for the benchmarker header record.

Written once as the first line of the results JSONL so every submission
carries its own hardware context, and recipients can group/filter without
trusting hand-annotated provenance.
"""

from __future__ import annotations

import platform
import socket
import subprocess
from datetime import UTC, datetime
from pathlib import Path


def _cpu_model() -> str | None:
    try:
        for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            key, _, value = line.partition(":")
            if key.strip() == "model name":
                return value.strip()
    except OSError:
        pass
    return None


def _cpu_cores() -> int | None:
    try:
        count = sum(
            1
            for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines()
            if line.startswith("processor")
        )
        return count or None
    except OSError:
        return None


def _ram_gb() -> float | None:
    try:
        import psutil

        return round(psutil.virtual_memory().total / 1e9, 1)
    except Exception:  # noqa: BLE001
        return None


def _gpu() -> tuple[str | None, float | None]:
    """(gpu_model, gpu_vram_gb) from nvidia-smi, or (None, None)."""
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader,nounits"],
            capture_output=True,
            text=True,
            timeout=5,
            check=True,
        ).stdout.strip()
        if out:
            name, _, mem = out.partition(",")
            return name.strip(), round(float(mem.strip()) / 1024, 1)
    except Exception:  # noqa: BLE001
        pass
    return None, None


def _ubuntu_version() -> str | None:
    try:
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            key, _, value = line.partition("=")
            if key.strip() == "PRETTY_NAME":
                return value.strip().strip('"')
    except OSError:
        pass
    return None


def collect() -> dict:
    """Return a machine-summary dict to write as the JSONL header record."""
    gpu_model, gpu_vram_gb = _gpu()
    return {
        "type": "machine",
        "hostname": socket.gethostname(),
        "cpu": _cpu_model(),
        "cpu_cores": _cpu_cores(),
        "ram_gb": _ram_gb(),
        "gpu": gpu_model,
        "gpu_vram_gb": gpu_vram_gb,
        "ubuntu": _ubuntu_version(),
        "kernel": platform.release(),
        "collected_at": datetime.now(UTC).isoformat(),
    }
