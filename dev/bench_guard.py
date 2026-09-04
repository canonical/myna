#!/usr/bin/env python3
"""Environment guard for inference benchmarks (parakeet perf T02).

    cd server && uv run python ../dev/bench_guard.py                    # human-readable
    cd server && uv run python ../dev/bench_guard.py --model whisper    # another profile
    cd server && uv run python ../dev/bench_guard.py --json             # machine-readable
    cd server && uv run python ../dev/bench_guard.py --force            # exit 0 regardless

Run from ``server/`` (like every other dev/ script here, see
``dev/parakeet/bench_parakeet.py``): the ``myna.server.lifecycle`` import below pulls
in the full installed package, so ``uv run`` needs ``server/pyproject.toml``'s
environment, not whatever (or nothing) resolves from the repo root.

Makes it impossible to record a benchmark number on a contaminated machine
without knowing it. The first pass of an early baseline was wrong by 18x
because the shell ran under an 800 MB cgroup cap against a 794 MB model: no
OOM, no warning, just plausible-looking numbers with a fake serial-scaling
wall. Every check here has a demonstrated failure mode.

Everything except the memory floor and the competing service is machine
policy, not model policy, so the checks are shared and only those two vary:
a ``Profile`` carries them per model family (see ``PROFILES``). It lives in
``dev/`` rather than under one model's directory because it is the second
model that proves it was never parakeet-specific - and because
``dev/spikes/parakeet_prefix_reuse.py`` already imported it from here.

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
import time
from dataclasses import asdict, dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

from myna.server.lifecycle import MAJOR_PAGE_FAULT_THRESHOLD, sample_majflt  # noqa: E402, F401

HARD = "hard"
WARN = "warn"

# Healthy is order 10^2 major faults per decode; the throttled baseline hit
# 202,792. 1000 sits comfortably above noise and far below the failure mode.
# Kept as an alias of the shared threshold (myna.server.lifecycle, T10) rather
# than a second literal, so the dev guard and the runtime detector agree by
# construction. Page-fault thrashing looks the same whatever the model, so
# this one is not per-profile.
MAX_MAJOR_PAGE_FAULTS = MAJOR_PAGE_FAULT_THRESHOLD

MEMORY_HEADROOM_BYTES = 512 * 1024**2


@dataclass(frozen=True)
class Profile:
    """The two things that are about the *model* rather than the machine.

    ``peak_rss_bytes`` is a measurement, not an estimate: take it from a real
    unthrottled run of the largest configuration the profile covers, because
    it is what both memory checks are calibrated against. ``min_cgroup_bytes``
    is deliberately well above it - the failure it guards against is silent
    reclaim, which starts biting long before the cap is actually reached.
    """

    name: str
    peak_rss_bytes: int
    min_cgroup_bytes: int
    weights_note: str  # what peak_rss_bytes was measured on, quoted in messages
    competing_snap: str | None = None
    competing_service: str | None = None


PROFILES = {
    # 794 MB encoder, 1.31 GB measured peak RSS (2026-08-28).
    "parakeet": Profile(
        name="parakeet",
        peak_rss_bytes=int(1.31 * 1024**3),
        min_cgroup_bytes=2 * 1024**3,
        weights_note="the 794 MB encoder (1.31 GB peak RSS)",
        competing_snap="myna-parakeet",
        competing_service="myna-parakeet.server",
    ),
    # Measured 2026-09-02, warm, one 5.9 s clip through model.transcribe on
    # the four Zen 5 cores: tiny/int8 317 MiB, base/float32 594 MiB,
    # small/float32 1328 MiB steady and 1541 MiB on the load that also wrote
    # the HF cache. The profile is sized for `small`, the largest CPU option
    # the snap offers, so one guard covers every model in the roster.
    "whisper": Profile(
        name="whisper",
        peak_rss_bytes=int(1.55 * 1024**3),
        min_cgroup_bytes=(5 * 1024**3) // 2,
        weights_note="whisper-small float32 (1.55 GB peak RSS)",
        competing_snap="myna-whisper",
        competing_service="myna-whisper.server",
    ),
}


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


def check_cgroup_memory(profile: Profile) -> list[Violation]:
    violations = []
    for scope in _cgroup_scope_paths():
        for name in ("memory.high", "memory.max"):
            limit = _read_limit(scope, name)
            if limit is not None and limit < profile.min_cgroup_bytes:
                violations.append(
                    Violation(
                        check="cgroup_memory",
                        severity=HARD,
                        message=(
                            f"{scope}/{name} = {limit} bytes ({limit / 1024**2:.0f} MiB), "
                            f"below the {profile.min_cgroup_bytes // 1024**2:.0f} MiB floor "
                            f"for {profile.weights_note}. This throttles "
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


def cpu_max_mhz(cpus: set[int] | None = None) -> float | None:
    """Nominal max clock of the affinity set, in MHz."""
    if cpus is None:
        try:
            cpus = os.sched_getaffinity(0)
        except (AttributeError, OSError):
            return None
    khz = []
    for cpu in cpus:
        try:
            khz.append(
                int(Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq").read_text())
            )
        except (OSError, ValueError):
            continue
    return max(khz) / 1000 if khz else None


def sample_cpu_mhz(cpus: set[int] | None = None) -> float | None:
    """Achieved clock right now, median over the affinity set, in MHz.

    The governor check above says the machine is *allowed* to boost; it says
    nothing about whether it did. On a laptop under sustained load it does
    not: measured 2026-09-02, the same benchmark that ran at 201 ms on a cool
    machine ran at 272 ms an hour later (**35% slower**) at 3.4% CV both
    times, with cpu0 sagging from ~4.9 GHz idle to 3.9-4.2 GHz mid-run. A
    tight CV proves a run was internally steady; it cannot see that the whole
    machine has moved. Record this alongside every number so cross-session
    comparisons are auditable instead of merely plausible.
    """
    if cpus is None:
        try:
            cpus = os.sched_getaffinity(0)
        except (AttributeError, OSError):
            return None
    khz = []
    for cpu in cpus:
        try:
            khz.append(
                int(Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq").read_text())
            )
        except (OSError, ValueError):
            continue
    if not khz:
        return None
    khz.sort()
    return khz[len(khz) // 2] / 1000


def check_competing_service(profile: Profile) -> list[Violation]:
    if profile.competing_snap is None or profile.competing_service is None:
        return []
    try:
        out = subprocess.run(
            ["snap", "services", profile.competing_snap],
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
        if len(fields) >= 3 and fields[0] == profile.competing_service and fields[2] == "active":
            return [
                Violation(
                    check="competing_service",
                    severity=HARD,
                    message=(
                        f"{profile.competing_service} is active, holding a warm copy of the "
                        "same weights and stealing CPU and page cache. Fix: "
                        f"sudo snap stop {profile.competing_snap} (restart with "
                        f"sudo snap start {profile.competing_snap} when done)"
                    ),
                )
            ]
    return []


def check_system_load() -> list[Violation]:
    """Judged against the *affinity set*, not the machine - and only a warning.

    Two corrections, in opposite directions, both from being wrong in
    practice on 2026-09-02.

    The denominator was wrong: comparing to ``os.cpu_count()`` (16 here) let a
    contaminated run through, because a benchmark pinned with
    ``taskset -c 0,2,4,6`` has four CPUs and a load average of 4.0 is already
    full contention. Three runs at 11-15% CV were taken before the cause was
    found. The pin is the whole point of the homogeneity check above, so this
    has to respect it.

    But making it a hard stop was also wrong, and it blocked a legitimate
    sweep within the hour: the 1-minute average decays over a minute, so it
    still carries *this benchmark's own* previous run and cannot tell "someone
    else is running now" from "I was measuring 30 seconds ago". A signal that
    cannot attribute load must not be able to refuse a run.
    ``check_competing_processes`` samples instantaneous per-process CPU and
    attributes it, so that one is the hard stop and this one is the hint.
    """
    try:
        load1, _, _ = os.getloadavg()
    except OSError:
        return []
    try:
        threshold = len(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        threshold = os.cpu_count() or 1
    # Our own runnable process is part of the load it is measuring.
    if load1 <= max(threshold - 1, 1):
        return []
    return [
        Violation(
            check="system_load",
            severity=WARN,
            message=(
                f"1-minute load average {load1:.2f} against {threshold} CPU(s) in this "
                "process's affinity set. If something else is competing for the cores "
                "being measured it inflates the median and the CV with no other symptom "
                "(ps -eo pcpu,pid,args --sort=-pcpu | head). If it is only this "
                "benchmark's own previous run still decaying out of the average, ignore "
                "it - and prefer the run's CV, which cannot be fooled that way."
            ),
        )
    ]


# Other inference servers holding weights and cores. Matched on the module
# invocation rather than the executable, because every worktree and venv
# spells the interpreter path differently.
COMPETING_PROCESS_MARKERS = ("myna.server", "myna-server")

# A competing process below this share of one core is resident but not
# actually taking cycles from the measured region. Idle servers are common on
# a shared box - several sessions keep one warm - and failing hard on those
# would make the guard permanently red, and therefore ignored.
COMPETING_CPU_BUSY_FRACTION = 0.05
_CPU_SAMPLE_SECONDS = 0.3


def _find_competing(me: int) -> list[tuple[int, str]]:
    found: list[tuple[int, str]] = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid == me:
            continue
        try:
            cmdline = (entry / "cmdline").read_bytes().decode("utf-8", "replace")
        except OSError:
            continue  # raced with exit, or not ours to read
        cmd = cmdline.replace("\0", " ").strip()
        if any(marker in cmd for marker in COMPETING_PROCESS_MARKERS):
            found.append((pid, cmd[:120]))
    return sorted(found)


def _cpu_ticks(pid: int) -> int | None:
    """utime + stime for a pid in clock ticks, or None if it went away."""
    try:
        stat = (Path("/proc") / str(pid) / "stat").read_text(encoding="utf-8")
    except OSError:
        return None
    # comm (field 2) can hold spaces and parentheses, so index from the last
    # ')': everything after it is fixed-width, with utime/stime at 12 and 13.
    try:
        fields = stat[stat.rindex(")") + 2 :].split()
        return int(fields[11]) + int(fields[12])
    except (ValueError, IndexError):
        return None


def check_competing_processes() -> list[Violation]:
    """Anything else on the box already serving a model.

    ``check_competing_service`` only sees snapd services. What actually bit
    this pass was nine ``python -m myna.server`` processes from a *different*
    checkout, started by a different session, taking three cores. No snap, no
    service, nothing for the snapd check to find.

    Severity follows what they are doing, sampled rather than assumed: one
    burning CPU is taking the cores being measured and is a hard stop, while
    one merely resident only holds page cache, which ``check_available_memory``
    already judges. Treating those the same makes the guard cry wolf on a
    machine where a warm server is normal, and a guard that is always red is
    a guard nobody reads.
    """
    found = _find_competing(os.getpid())
    if not found:
        return []

    hz = os.sysconf("SC_CLK_TCK")
    before = {pid: _cpu_ticks(pid) for pid, _ in found}
    time.sleep(_CPU_SAMPLE_SECONDS)
    busy: list[tuple[int, str, float]] = []
    for pid, cmd in found:
        start_ticks, end_ticks = before.get(pid), _cpu_ticks(pid)
        if start_ticks is None or end_ticks is None:
            continue
        share = (end_ticks - start_ticks) / hz / _CPU_SAMPLE_SECONDS
        if share >= COMPETING_CPU_BUSY_FRACTION:
            busy.append((pid, cmd, share))

    if busy:
        detail = "; ".join(f"pid {pid} at {share:.0%} of a core: {cmd}" for pid, cmd, share in busy)
        return [
            Violation(
                check="competing_processes",
                severity=HARD,
                message=(
                    f"{len(busy)} other inference server process(es) actively using CPU: "
                    f"{detail}. They are taking the cores being measured, which inflates "
                    "the median and the CV with no other symptom. Wait for whoever started "
                    "them - they are not yours to kill."
                ),
            )
        ]

    detail = "; ".join(f"pid {pid}: {cmd}" for pid, cmd in found[:3])
    more = f" (and {len(found) - 3} more)" if len(found) > 3 else ""
    return [
        Violation(
            check="competing_processes",
            severity=WARN,
            message=(
                f"{len(found)} other inference server process(es) resident but idle: "
                f"{detail}{more}. Not taking CPU right now, so a run is valid, but they "
                "hold weights in the page cache and can wake at any time - re-run the "
                "guard after measuring if a number looks surprising."
            ),
        )
    ]


def check_available_memory(profile: Profile) -> list[Violation]:
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
    needed = profile.peak_rss_bytes + MEMORY_HEADROOM_BYTES
    if available >= needed:
        return []
    return [
        Violation(
            check="available_memory",
            severity=WARN,
            message=(
                f"MemAvailable {available / 1024**3:.2f} GiB is under the "
                f"{needed / 1024**3:.2f} GiB wanted for {profile.weights_note} plus "
                "headroom. Close other applications, or expect reclaim pressure of the "
                "benchmark's own making."
            ),
        )
    ]


def check(profile: Profile, cpus: set[int] | None = None) -> list[Violation]:
    """All pre-run checks. Does not include major page faults, which can only
    be judged after the measured region -- see ``check_page_faults``.

    ``profile`` is required rather than defaulted: a caller that inherits
    another model's memory floor by omission gets a guard that passes on a
    machine it should have refused, which is the failure this module exists
    to make impossible."""
    violations: list[Violation] = []
    violations += check_cgroup_memory(profile)
    violations += check_cpu_governor()
    violations += check_core_homogeneity(cpus)
    violations += check_competing_service(profile)
    violations += check_competing_processes()
    violations += check_system_load()
    violations += check_available_memory(profile)
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
    ap.add_argument(
        "--model",
        choices=sorted(PROFILES),
        default="parakeet",
        help="which model family's memory floor and competing service to check",
    )
    args = ap.parse_args()

    violations = check(PROFILES[args.model])
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
