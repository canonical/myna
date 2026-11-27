"""The AppArmor denial gate (tests/lib/apparmor_denials.py) over real journal lines."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.repo_tree

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "tests" / "lib"))

import apparmor_denials as ad  # noqa: E402

KERNEL = (
    "Oct 16 21:31:11.648432 framework kernel: audit: type=1400 audit(1792182671.646:82798): "
    'apparmor="DENIED" operation="open" class="file" profile="snap.myna-parakeet.server" '
    'name="/sys/fs/cgroup/system.slice/memory.high" pid=1234723 comm="python3" '
    'requested_mask="r" denied_mask="r" fsuid=0 ouid=0'
)
CAPABLE = (
    "Oct 16 21:30:02.100000 framework kernel: audit: type=1400 audit(1792182602.100:82700): "
    'apparmor="DENIED" operation="capable" class="cap" profile="snap.myna-whisper.hook.install" '
    'pid=810874 comm="install" capability=1  capname="dac_override"'
)
FOREIGN_NAMESPACE = (
    'Oct 16 21:30:02 framework kernel: audit: apparmor="DENIED" operation="capable" class="cap" '
    'namespace="root//lxd-workshop.charles_myna-754c33df_<var-snap-lxd-common-lxd>" '
    'profile="systemd-detect-virt" pid=1883892 comm="systemd-detect-" capability=1  '
    'capname="dac_override"'
)


def dbus(label: str, peer: str, unique: str = ":1.2752") -> str:
    return (
        "Oct 13 00:48:37.745916 framework dbus-broker[2604201]: "
        'apparmor="DENIED" operation="dbus_signal" bus="session" '
        'path="/com/canonical/Myna/Dictation" interface="org.freedesktop.DBus.Properties" '
        f'member="PropertiesChanged" mask="send" name="{unique}" pid=6562 '
        f'label="{label}" peer_pid=81266 peer_label="{peer}"'
    )


def test_kernel_denial_is_ours():
    d = ad.parse(KERNEL)
    assert d is not None
    assert d["subject"] == "snap.myna-parakeet.server"
    assert d["name"] == "/sys/fs/cgroup/system.slice/memory.high"


def test_capability_denial_keeps_the_capability():
    d = ad.parse(CAPABLE)
    assert d is not None
    assert "capname=dac_override" in ad.key(d)
    assert "comm=install" in ad.key(d)


@pytest.mark.parametrize(
    "line",
    [
        FOREIGN_NAMESPACE,
        KERNEL.replace("snap.myna-parakeet.server", "snap.firefox.firefox"),
        KERNEL.replace("snap.myna-parakeet.server", "snap.mynaother.app"),
        dbus("snap.firefox.firefox", "snap.myna.myna"),
        KERNEL.replace('apparmor="DENIED"', 'apparmor="ALLOWED"'),
        "Oct 13 00:48:37 framework systemd[1]: Started snap.myna.myna.service.",
    ],
)
def test_other_lines_are_ignored(line):
    assert ad.parse(line) is None


def test_unique_bus_names_do_not_split_a_denial():
    unexpected, _ = ad.check(
        [dbus("snap.myna.hud", "unconfined", ":1.1"), dbus("snap.myna.hud", "unconfined", ":1.9")]
    )
    assert list(unexpected.values()) == [2]


def test_named_unconfined_peer_is_allowed_on_our_path():
    unexpected, allowed = ad.check([dbus("snap.myna.myna", "desktop-icons-ng")])
    assert not unexpected
    assert sum(allowed.values()) == 1


def kernel(profile: str, name: str, comm: str = "python3") -> str:
    return (
        KERNEL.replace("snap.myna-parakeet.server", profile)
        .replace("/sys/fs/cgroup/system.slice/memory.high", name)
        .replace('comm="python3"', f'comm="{comm}"')
    )


def unlink(profile: str, name: str, comm: str = "python3") -> str:
    return kernel(profile, name, comm).replace('operation="open"', 'operation="unlink"')


PORTAL = (
    'Oct 14 17:37:52.161008 framework dbus-broker[1213517]: apparmor="DENIED" '
    'operation="dbus_method_call" bus="session" path="/org/freedesktop/portal/desktop" '
    'interface="org.freedesktop.host.portal.Registry" member="Register" mask="send" '
    'name="org.freedesktop.portal.Desktop" pid=2 label="snap.myna.hud" peer_pid=3 '
    'peer_label="unconfined"'
)


PERFMON = (
    "Nov 25 23:12:25.203114 chug kernel: audit: type=1400 audit(1795649545.200:30806): "
    'apparmor="DENIED" operation="capable" class="cap" profile="snap.myna-whisper.server" '
    'pid=1454745 comm="python3" capability=38  capname="perfmon"'
)


RTKIT = (
    "Oct 20 20:24:04.647567 myna-e2e-noble-run kernel: audit: type=1107 "
    "audit(1792527844.642:191): pid=835 uid=101 auid=4294967295 ses=4294967295 "
    'subj=unconfined msg=\'apparmor="DENIED" operation="dbus_method_call"  bus="system" '
    'path="/org/freedesktop/RealtimeKit1" interface="org.freedesktop.RealtimeKit1" '
    'member="MakeThreadRealtimeWithPID" mask="send" name="org.freedesktop.RealtimeKit1" '
    'pid=2647 label="snap.myna.myna" peer_pid=1330 peer_label="unconfined"'
)


@pytest.mark.parametrize(
    "line",
    [
        kernel("snap.myna.testbed", "/sys/fs/cgroup/user.slice/user-1000.slice/cpu.max"),
        kernel("snap.myna.myna", "/sys/fs/cgroup/cpu.max"),
        kernel("snap.myna-whisper.whisper", "/proc/sys/kernel/arch", "whisper"),
        kernel("snap.myna.hud", "/etc/vulkan/icd.d/", "myna-hud"),
        kernel("snap.myna.hud", "/etc/vulkan/implicit_layer.d/", "myna-hud"),
        kernel("snap.myna-whisper.server", "/proc/1234723/environ"),
        unlink("snap.myna-whisper.server", "/dev/char/195:255"),
        unlink("snap.myna-parakeet.server", "/dev/char/507:1"),
        unlink("snap.myna.hud", "/dev/char/195:0", "myna-hud"),
        kernel(
            "snap.myna-whisper.whisper",
            "/proc/driver/nvidia/capabilities/mig/config",
            "nvidia-smi",
        ),
        PERFMON,
        PORTAL,
        kernel("snap.myna-text.server", "/sys/bus/pci/devices/", "python3"),
        kernel(
            "snap.myna-text.server",
            "/sys/devices/pci0000:00/0000:00:01.3/0000:04:00.0/vendor",
            "python3",
        ),
        kernel(
            "snap.myna.myna", "/var/snap/myna-translation/common/share/provider/text.sock"
        ).replace('operation="open"', 'operation="connect"'),
    ],
)
def test_known_harmless_denials_are_allowed(line):
    unexpected, allowed = ad.check([line])
    assert not unexpected
    assert sum(allowed.values()) == 1


@pytest.mark.parametrize(
    "line",
    [
        kernel("snap.myna.testbed", "/sys/fs/cgroup/user.slice/memory.max"),
        kernel("snap.myna.testbed", "/sys/fs/cgroup/user.slice/cpu.max.evil"),
        kernel("snap.myna-whisper.server", "/etc/vulkan/icd.d/"),
        kernel("snap.myna.hud", "/etc/vulkan/icd.d/nvidia.json", "myna-hud"),
        unlink("snap.myna-whisper.server", "/dev/char/195:255/../../etc/shadow"),
        kernel("snap.myna-whisper.server", "/dev/char/195:255"),
        unlink("snap.myna-whisper.server", "/dev/nvidia0"),
        kernel("snap.myna-whisper.server", "/dev/nvidia0"),
        kernel("snap.myna-whisper.whisper", "/proc/driver/nvidia/params"),
        PERFMON.replace("perfmon", "sys_admin"),
        # Another process's environment, not its own.
        kernel("snap.myna-whisper.server", "/proc/1/environ"),
        kernel("snap.myna-whisper.server", "/proc/1234723/cmdline"),
        kernel("snap.myna-text.server", "/sys/bus/usb/devices/", "python3"),
        kernel("snap.myna-translation.server", "/sys/bus/pci/devices/", "python3"),
        PORTAL.replace("snap.myna.hud", "snap.myna.myna"),
        PORTAL.replace('member="Register"', 'member="Unregister"'),
        # Capture falling back to RealtimeKit has lost realtime (myna#76).
        RTKIT,
        kernel(
            "snap.myna.myna", "/var/snap/myna-translation/common/share/provider/other.sock"
        ).replace('operation="open"', 'operation="connect"'),
        kernel("snap.myna.myna", "/var/snap/myna-translation/common/share/provider/text.sock"),
        kernel(
            "snap.myna-whisper.server", "/var/snap/myna-translation/common/share/provider/text.sock"
        ).replace('operation="open"', 'operation="connect"'),
        # Hooks log through pipes since 5dae45b8, which 0.3.0 ships.
        CAPABLE,
        dbus("snap.myna.myna", "unconfined"),
        dbus("snap.myna.myna", "snap.myna.hud"),
        dbus("snap.myna.hud", "desktop-icons-ng"),
        dbus("snap.myna.myna", "desktop-icons-ng").replace(
            "/com/canonical/Myna/Dictation", "/org/freedesktop/portal/desktop"
        ),
    ],
)
def test_allowlist_does_not_reach_past_its_case(line):
    unexpected, allowed = ad.check([line])
    assert sum(unexpected.values()) == 1
    assert not allowed


def test_main_fails_on_an_unexpected_denial(tmp_path, capsys):
    journal = tmp_path / "journal.log"
    journal.write_text("\n".join([KERNEL, KERNEL, dbus("snap.myna.myna", "desktop-icons-ng")]))
    assert ad.main([str(journal)]) == 1
    out = capsys.readouterr().out
    assert "DENIED x2: subject=snap.myna-parakeet.server comm=python3 operation=open" in out
    assert "allowed x1: " in out
    assert "FAIL: 1 unexpected" in out


def test_main_passes_on_a_clean_journal(tmp_path, capsys):
    journal = tmp_path / "journal.log"
    journal.write_text("\n".join([FOREIGN_NAMESPACE, dbus("snap.myna.myna", "desktop-icons-ng")]))
    assert ad.main([str(journal)]) == 0
    assert "FAIL" not in capsys.readouterr().out
