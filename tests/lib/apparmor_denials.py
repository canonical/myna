#!/usr/bin/env python3
"""Fail on AppArmor denials of Myna's snaps in a journal.

Reads journal text on stdin (or from the files named) and keeps the
`apparmor="DENIED"` lines whose subject is one of our snaps: kernel audit
(`profile=`) and the D-Bus broker (`label=`). Every such denial must match an
ALLOWED entry, or the run fails and the unexpected ones are printed.

The caller picks the window: a journal cursor, or the boot of a throwaway
machine. Myna's lines from before the test would otherwise count.

Usage: journalctl --cursor-file=F | apparmor_denials.py
       apparmor_denials.py journal.log
"""

from __future__ import annotations

import fileinput
import re
import sys
from collections import Counter
from dataclasses import dataclass

OURS = re.compile(r"snap\.myna(-[a-z0-9]+)?\..+")
FIELD = re.compile(r'(\w+)=("[^"]*"|\S+)')
# What tells two denials apart; pids, timestamps and serials do not.
KEY_FIELDS = (
    "subject",
    "comm",
    "operation",
    "capname",
    "name",
    "path",
    "interface",
    "member",
    "mask",
    "denied_mask",
    "peer_label",
)


@dataclass(frozen=True)
class Allowed:
    """A denial we accept, with why and where it is tracked."""

    match: dict[str, str]  # field -> regex the whole value must match
    reason: str

    def matches(self, denial: dict[str, str]) -> bool:
        return all(
            re.fullmatch(pattern, denial.get(field, "")) for field, pattern in self.match.items()
        )


ALLOWED: tuple[Allowed, ...] = (
    Allowed(
        {
            "subject": r"snap\.myna\.myna",
            "operation": r"dbus_.*",
            "path": r"/com/canonical/Myna/Dictation(/.*)?",
            "peer_label": r"(?!unconfined$|snap\.).+",
        },
        "snapd's dbus slot admits only label=unconfined peers (LP: #2171555, myna#53)",
    ),
    Allowed(
        {"operation": "open", "name": r"/sys/fs/cgroup/(.+/)?cpu\.max"},
        "Rust std's available_parallelism walks every ancestor cgroup's cpu.max"
        " and skips what it cannot read",
    ),
    Allowed(
        {"operation": "open", "name": r"/proc/sys/kernel/arch"},
        "snapd's base template grants osrelease and ostype but not arch;"
        " modelctl's lscompute falls back to uname(2)",
    ),
    Allowed(
        {"operation": "open", "name": r"/proc/self/environ"},
        "Intel MKL, linked into CTranslate2, reads INTEL_ISA_DISABLE from its own"
        " environ and takes it as unset; snapd's base template lacks the rule",
    ),
    Allowed(
        {"operation": "unlink", "name": r"/dev/char/\d+:\d+"},
        "NVIDIA's user-space libraries (CUDA, nvidia-smi) try to replace their"
        " /dev/char/<major>:<minor> symlinks, which the driver package already"
        " installed; snapd's opengl interface grants /dev/nvidia* and nothing"
        " under /dev/char, as it should. The major of nvidia-uvm is assigned at"
        " boot. CUDA runs and transcribes regardless",
    ),
    Allowed(
        {"operation": "open", "name": r"/proc/driver/nvidia/capabilities/mig/config"},
        "nvidia-smi, which modelctl runs for the compute capability, reads the"
        " MIG configuration; the capability is still reported",
    ),
    Allowed(
        {"operation": "capable", "capname": "perfmon"},
        "CUDA initialisation checks CAP_PERFMON once (GPU performance counters);"
        " the context is created and serves without it",
    ),
    Allowed(
        {
            "subject": r"snap\.myna\.hud",
            "operation": "open",
            "name": r"/etc/vulkan/(icd|implicit_layer|explicit_layer)\.d/",
        },
        "the Vulkan loader lists the host's /etc/vulkan, which libvulkan1 ships"
        " empty; the HUD's drivers come from gpu-2404",
    ),
    Allowed(
        {
            "subject": r"snap\.myna\.hud",
            "operation": "dbus_method_call",
            "interface": r"org\.freedesktop\.host\.portal\.Registry",
            "member": "Register",
        },
        "GTK before f4206d04 takes a snap for unsandboxed and registers it with"
        " the portal, ignoring the error",
    ),
    Allowed(
        {
            "subject": r"snap\.myna-text\.server",
            "operation": "open",
            "name": r"/sys/(bus/pci/devices/|devices/pci[^ ]*/vendor)",
        },
        "onnxruntime scans PCI devices at start; the store does not auto-connect"
        " myna-text:hardware-observe yet, and the server starts inside Settings'"
        " install, before any connect (delete once the store grants it)",
    ),
    Allowed(
        {
            "subject": r"snap\.myna\.myna",
            "operation": "connect",
            "name": r"/var/snap/myna-(text|translation)/common/share/provider/text\.sock",
        },
        "`snap connect myna:backend <text snap>:provider` remounts the share, the daemon"
        " exits and systemd restarts it before snapd replaces snap.myna.myna's profile"
        " with the new connect rule; it reads the provider as unavailable and binds it"
        " at its next start",
    ),
)


def parse(line: str) -> dict[str, str] | None:
    """The fields of one of our denials, or None for any other line."""
    if 'apparmor="DENIED"' not in line:
        return None
    fields = {k: v.strip('"') for k, v in FIELD.findall(line.split('apparmor="DENIED"', 1)[1])}
    subject = fields.get("profile") or fields.get("label", "")
    if not OURS.fullmatch(subject):
        return None
    fields["subject"] = subject
    if fields.get("name", "").startswith(":"):
        fields["name"] = ":unique"
    pid = fields.get("pid")
    if pid and fields.get("name", "").startswith(f"/proc/{pid}/"):
        fields["name"] = "/proc/self/" + fields["name"][len(f"/proc/{pid}/") :]
    return fields


def key(denial: dict[str, str]) -> tuple[str, ...]:
    return tuple(f"{f}={denial[f]}" for f in KEY_FIELDS if f in denial)


def check(lines) -> tuple[Counter, Counter]:
    """Unexpected denials by key(), allowed ones by the reason that admits them."""
    unexpected: Counter = Counter()
    allowed: Counter = Counter()
    for line in lines:
        denial = parse(line)
        if denial is None:
            continue
        hit = next((a for a in ALLOWED if a.matches(denial)), None)
        if hit is None:
            unexpected[key(denial)] += 1
        else:
            allowed[hit.reason] += 1
    return unexpected, allowed


def main(argv: list[str]) -> int:
    unexpected, allowed = check(fileinput.input(argv, encoding="utf-8", errors="replace"))
    for reason, n in sorted(allowed.items()):
        print(f"allowed x{n}: {reason}")
    for denial, n in sorted(unexpected.items()):
        print(f"DENIED x{n}: {' '.join(denial)}")
    if unexpected:
        print(f"FAIL: {len(unexpected)} unexpected AppArmor denial(s) of Myna's snaps")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
