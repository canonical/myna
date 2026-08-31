#!/usr/bin/env python3
"""Environment guard for Parakeet benchmarks (perf T02).

    cd server && uv run python ../dev/parakeet/bench_guard.py          # human-readable
    cd server && uv run python ../dev/parakeet/bench_guard.py --json    # machine-readable
    cd server && uv run python ../dev/parakeet/bench_guard.py --force   # exit 0 regardless

Run from ``server/`` (like every other dev/ script here, see
``dev/parakeet/bench_parakeet.py``): the ``myna.server.lifecycle`` import below pulls
in the full installed package, so ``uv run`` needs ``server/pyproject.toml``'s
environment, not whatever (or nothing) resolves from the repo root.

Makes it impossible to record a benchmark number on a contaminated machine
without knowing it. The first pass of an early baseline was wrong by 18x
because the shell ran under an 800 MB cgroup cap against a 794 MB model: no
OOM, no warning, just plausible-looking numbers with a fake serial-scaling
wall. Every check here has a demonstrated failure mode.

``sample_majflt`` (the page-fault sampling primitive) and its threshold live
in ``myna.server.lifecycle`` instead of here (T10, runtime memory-pressure
detection): that module is part of the installed package and importable from
a packaged snap, which this ``dev/`` script is not (see
``myna.testbed.parakeet._default_model_dir`` for the same dev/package split).
Importing it from there, rather than each keeping its own copy, is what keeps
the dev-time guard and the runtime detector from drifting on what "a major
fault" means.

Public surface:
    Violation           -- one failed or warned check, with its one-line fix.
    check()              -- all pre-run checks (everything but page faults).
    sample_majflt()       -- current process's major-fault counter (re-exported).
    check_page_faults()   -- post-run check: call with before/after samples.

Fixing the environment is out of scope on purpose: writing to
``scaling_governor`` or ``memory.high`` from a benchmark tool is a surprising
side effect on a shared machine. The guard reports; the operator decides.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

from myna.server.lifecycle import MAJOR_PAGE_FAULT_THRESHOLD, sample_majflt  # noqa: E402, F401

HARD = "hard"
WARN = "warn"

# 794 MB encoder, 1.31 GB measured peak RSS.
# A limit below this throttles silently instead of OOMing -- that is the 18x
# bug. Round well above peak RSS so the check fires before things get close.
MIN_CGROUP_MEMORY_BYTES = 2 * 1024**3

# Healthy is order 10^2 major faults per decode; the throttled baseline hit
# 202,792. 1000 sits comfortably above noise and far below the failure mode.
# Kept as an alias of the shared threshold (myna.server.lifecycle, T10) rather
# than a second literal, so the dev guard and the runtime detector agree by
# construction.
MAX_MAJOR_PAGE_FAULTS = MAJOR_PAGE_FAULT_THRESHOLD

COMPETING_SNAP = "myna-parakeet"
COMPETING_SERVICE = "myna-parakeet.server"

# Peak RSS, unthrottled, measured 2026-08-28.
PEAK_RSS_BYTES = int(1.31 * 1024**3)
MEMORY_HEADROOM_BYTES = 512 * 1024**2


@dataclass(frozen=True)
class Violation:
    """One guard check that failed, with the fix inline so nobody has to go
    hunting for it while a benchmark is stalled."""

    check: str
    severity: str  # HARD or WARN
    message: str

    def __str__(self) -> str:
        tag = "HARD" if self.severity == HARD else "warn"
        return f"[{tag}] {self.check}: {self.message}"


def _cgroup_scope_paths() -> list[Path]:
    """Own cgroup v2 scope plus every ancestor slice, nearest first, stopping
    at (excluding) the cgroup root -- the limit that bit us was on the scope,
    but a limit on any ancestor slice has the same effect."""
    try:
        line = Path("/proc/self/cgroup").read_text(encoding="utf-8").strip().splitlines()[0]
    except (FileNotFoundError, IndexError, OSError):
        return []
    parts = line.split(":", 2)
    if len(parts) != 3:
        return []
    root = Path("/sys/fs/cgroup")
    scope = root / parts[2].lstrip("/")
    paths = []
    p = scope
    while p != root and p != p.parent:
        paths.append(p)
        p = p.parent
    return paths


def _read_limit(path: Path, name: str) -> int | None:
    """Bytes, or None if the file is absent, unreadable, or ``max`` (unlimited)."""
    try:
        text = (path / name).read_text(encoding="utf-8").strip()
    except OSError:
        return None
    if text == "max":
        return None
    try:
        return int(text)
    except ValueError:
        return None


def check_cgroup_memory() -> list[Violation]:
    violations = []
    for scope in _cgroup_scope_paths():
        for name in ("memory.high", "memory.max"):
            limit = _read_limit(scope, name)
            if limit is not None and limit < MIN_CGROUP_MEMORY_BYTES:
                violations.append(
                    Violation(
                        check="cgroup_memory",
                        severity=HARD,
                        message=(
                            f"{scope}/{name} = {limit} bytes ({limit / 1024**2:.0f} MiB), "
                            f"below the {MIN_CGROUP_MEMORY_BYTES // 1024**2:.0f} MiB floor "
                            "for the 794 MB encoder (1.31 GB peak RSS). This throttles "
                            "silently instead of OOMing -- 18x error, no warning, no OOM "
                            "kill, high major-fault count. Fix: "
                            f"echo max | sudo tee {scope}/{name}"
                        ),
                    )
                )
    return violations


def check_cpu_governor() -> list[Violation]:
    bad = []
    governor_files = Path("/sys/devices/system/cpu").glob("cpu[0-9]*/cpufreq/scaling_governor")
    for gov_file in sorted(governor_files):
        try:
            gov = gov_file.read_text(encoding="utf-8").strip()
        except OSError:
            continue
        if gov != "performance":
            bad.append((gov_file.parts[-3], gov))
    if not bad:
        return []
    detail = ", ".join(f"{cpu}={gov}" for cpu, gov in bad)
    return [
        Violation(
            check="cpu_governor",
            severity=HARD,
            message=(
                f"non-performance governor on {detail}. Default powersave makes short "
                "runs unrepeatable. Fix: for g in "
                "/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; "
                "do echo performance | sudo tee $g; done"
            ),
        )
    ]


def check_core_homogeneity(cpus: set[int] | None = None) -> list[Violation]:
    """Fails both for an explicitly bad pin and for no pin at all: default
    affinity spans every logical CPU, which on this part means both the
    5.09 GHz Zen 5 cores and the 3.51 GHz Zen 5c cores -- up to 1.45x variance
    from scheduler placement alone."""
    if cpus is None:
        try:
            cpus = os.sched_getaffinity(0)
        except (AttributeError, OSError):
            return []
    freqs: dict[int, int] = {}
    for cpu in sorted(cpus):
        f = Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq")
        try:
            freqs[cpu] = int(f.read_text(encoding="utf-8").strip())  # kHz
        except OSError:
            continue
    if len({*freqs.values()}) <= 1:
        return []
    detail = ", ".join(f"cpu{c}={khz / 1e6:.2f}GHz" for c, khz in sorted(freqs.items()))
    return [
        Violation(
            check="core_homogeneity",
            severity=HARD,
            message=(
                f"affinity set spans more than one core frequency ({detail}). Interleaved "
                "fast/slow cores inject up to 1.45x variance from placement alone. Fix: "
                "taskset -c 0,2,4,6 <cmd> (the four 5.09 GHz Zen 5 cores; verify per-machine "
                "with cat /sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq)"
            ),
        )
    ]


def check_competing_service() -> list[Violation]:
    try:
        out = subprocess.run(
            ["snap", "services", COMPETING_SNAP],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []  # snapd absent or unresponsive: nothing competing we can see
    if out.returncode != 0:
        return []  # snap not installed on this machine
    for line in out.stdout.splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 3 and fields[0] == COMPETING_SERVICE and fields[2] == "active":
            return [
                Violation(
                    check="competing_service",
                    severity=HARD,
                    message=(
                        f"{COMPETING_SERVICE} is active, holding a warm ~1.6 GB copy of the "
                        "same encoder weights and stealing CPU and page cache. Fix: "
                        f"sudo snap stop {COMPETING_SNAP} (restart with "
                        f"sudo snap start {COMPETING_SNAP} when done)"
                    ),
                )
            ]
    return []


def check_system_load() -> list[Violation]:
    try:
        load1, _, _ = os.getloadavg()
    except OSError:
        return []
    threshold = os.cpu_count() or 1
    if load1 <= threshold:
        return []
    return [
        Violation(
            check="system_load",
            severity=WARN,
            message=(
                f"1-minute load average {load1:.2f} exceeds {threshold} logical CPUs. "
                "Something else on this machine is competing for CPU; check top/htop and "
                "close it before trusting a tight CV."
            ),
        )
    ]


def check_available_memory() -> list[Violation]:
    try:
        meminfo = Path("/proc/meminfo").read_text(encoding="utf-8")
    except OSError:
        return []
    available_kb = None
    for line in meminfo.splitlines():
        if line.startswith("MemAvailable:"):
            available_kb = int(line.split()[1])
            break
    if available_kb is None:
        return []
    available = available_kb * 1024
    needed = PEAK_RSS_BYTES + MEMORY_HEADROOM_BYTES
    if available >= needed:
        return []
    return [
        Violation(
            check="available_memory",
            severity=WARN,
            message=(
                f"MemAvailable {available / 1024**3:.2f} GiB is under the "
                f"{needed / 1024**3:.2f} GiB wanted for the model's 1.31 GB peak RSS plus "
                "headroom. Close other applications, or expect reclaim pressure of the "
                "benchmark's own making."
            ),
        )
    ]


def check(cpus: set[int] | None = None) -> list[Violation]:
    """All pre-run checks. Does not include major page faults, which can only
    be judged after the measured region -- see ``check_page_faults``."""
    violations: list[Violation] = []
    violations += check_cgroup_memory()
    violations += check_cpu_governor()
    violations += check_core_homogeneity(cpus)
    violations += check_competing_service()
    violations += check_system_load()
    violations += check_available_memory()
    return violations


def check_page_faults(before: int, after: int) -> Violation | None:
    """Necessarily post-run: pass ``sample_majflt()`` taken immediately before
    and immediately after the measured region."""
    delta = after - before
    if delta <= MAX_MAJOR_PAGE_FAULTS:
        return None
    return Violation(
        check="major_page_faults",
        severity=HARD,
        message=(
            f"{delta} major page faults during the measured region, over the "
            f"{MAX_MAJOR_PAGE_FAULTS} limit (healthy is order 10^2; the throttled 2026-08-28 "
            "baseline hit 202,792). This is the fingerprint of a memory cap the process "
            "cannot fit under. Fix: re-run check_cgroup_memory -- lift it with "
            "CG=/sys/fs/cgroup$(awk -F: '{print $3}' /proc/self/cgroup | head -1); "
            "echo max | sudo tee $CG/memory.high -- and if that is already lifted, check "
            "free -h for real memory pressure."
        ),
    )


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--force", action="store_true", help="exit 0 even if hard violations were found"
    )
    ap.add_argument("--json", action="store_true", help="print violations as a JSON array")
    args = ap.parse_args()

    violations = check()
    hard = [v for v in violations if v.severity == HARD]

    if args.json:
        print(json.dumps([asdict(v) for v in violations], indent=2))
    else:
        if not violations:
            print("environment: clean")
        else:
            for v in violations:
                print(v)

    if hard and not args.force:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
