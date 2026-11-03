#!/usr/bin/python3
"""Dictations through the running Myna daemon that must not lose audio.

Run by continuity.sh as the lab machine's user, inside its lingering user
manager. Holds an IBus input context of its own focused as the text field,
starts and stops each dictation with the daemon's D-Bus Toggle, and fails on
any `ctrl: capture lost` from the daemon, on a dictation that does not
record for its whole length, or when capture or PipeWire is not realtime.
A last dictation freezes the daemon on purpose and must lose audio, so a
pass means the detector works on this machine.
"""

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import gi

gi.require_version("IBus", "1.0")
from gi.repository import Gio, GLib, IBus  # noqa: E402

NAME = "com.canonical.Myna.Dictation"
PATH = "/com/canonical/Myna/Dictation"
UNIT = "snap.myna.myna.service"
LOST = "ctrl: capture lost"
SCHED = {0: "OTHER", 1: "FIFO", 2: "RR", 3: "BATCH", 5: "IDLE", 6: "DEADLINE"}


def log(msg):
    print(f"continuity: {msg}", flush=True)


def start_ibus():
    subprocess.run(
        ["ibus-daemon", "--xim=no", "--panel=disable", "--replace", "--daemonize"], check=True
    )
    deadline = time.monotonic() + 20
    while subprocess.run(["ibus", "list-engine"], capture_output=True).returncode:
        if time.monotonic() > deadline:
            sys.exit("continuity: no IBus daemon")
        time.sleep(0.5)


def field():
    """The focused text field: an input context of our own, printing commits."""
    IBus.init()
    bus = IBus.Bus()
    if not bus.is_connected():
        sys.exit("continuity: cannot connect to the IBus daemon")
    ic = bus.create_input_context("myna-continuity")
    ic.set_capabilities(
        IBus.Capabilite.FOCUS | IBus.Capabilite.PREEDIT_TEXT | IBus.Capabilite.SURROUNDING_TEXT
    )
    ic.connect("commit-text", lambda _ic, text: print("commit", text.get_text(), flush=True))
    ic.focus_in()
    print("ready", flush=True)
    GLib.MainLoop().run()


def hold_field():
    child = subprocess.Popen(
        [sys.executable, "-I", __file__, "--field"], stdout=subprocess.PIPE, text=True
    )
    if child.stdout.readline().strip() != "ready":
        sys.exit("continuity: the field did not take focus")
    return child


class Daemon:
    def __init__(self):
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION)

    def _call(self, iface, method, args=None):
        return self.bus.call_sync(
            NAME, PATH, iface, method, args, None, Gio.DBusCallFlags.NONE, 5000, None
        )

    def state(self):
        try:
            reply = self._call(
                "org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (NAME, "State"))
            )
        except GLib.Error:
            return None
        return reply.unpack()[0]

    def toggle(self):
        self._call(NAME, "Toggle")

    def wait(self, pred, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            s = self.state()
            if pred(s):
                return s
            time.sleep(0.1)
        return self.state()


def unit_pid(unit):
    out = subprocess.run(
        ["systemctl", "--user", "show", "-p", "MainPID", "--value", unit],
        capture_output=True,
        text=True,
        check=True,
    )
    return int(out.stdout.strip())


def threads(pid):
    """(name, policy, priority) of each thread of `pid`."""
    found = []
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            comm = (task / "comm").read_text().strip()
            fields = (task / "stat").read_text().rsplit(")", 1)[1].split()
        except OSError:
            continue
        # stat fields 40 and 41 (rt_priority, policy), counted after comm.
        found.append((comm, SCHED.get(int(fields[38]), fields[38]), int(fields[37])))
    return found


def realtime(pid):
    return [t for t in threads(pid) if t[1] in ("FIFO", "RR")]


def losses(since):
    out = subprocess.run(
        ["journalctl", "--user", "-u", UNIT, "--since", since, "--no-pager", "-o", "cat"],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line for line in out.stdout.splitlines() if LOST in line]


def dictate(daemon, seconds, during=None):
    """One dictation of `seconds`; returns a failure or None."""
    daemon.toggle()
    if daemon.wait(lambda s: s == "recording", 10) != "recording":
        return f"did not start recording (state {daemon.state()})"
    if during:
        during()
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        s = daemon.state()
        if s != "recording":
            return f"stopped recording early (state {s})"
        time.sleep(0.2)
    daemon.toggle()
    s = daemon.wait(lambda s: s in ("idle", "error"), 60)
    if s != "idle":
        return f"ended {s}"
    return None


def busy(n):
    return [subprocess.Popen(["sh", "-c", "while :; do :; done"]) for _ in range(n)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--field", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--seconds", type=float, default=20)
    args = ap.parse_args()
    if args.field:
        return field()

    start_ibus()
    held = hold_field()
    # The daemon's module-rt asks the realtime portal only if it is already
    # running, as a desktop session has it; confined, it cannot reach rtkit
    # itself (#76). Then start the daemon afresh with IBus and the portal up.
    subprocess.run(["systemctl", "--user", "start", "xdg-desktop-portal.service"], check=True)
    subprocess.run(["systemctl", "--user", "restart", UNIT], check=True)
    daemon = Daemon()
    if daemon.wait(lambda s: s == "idle", 30) != "idle":
        sys.exit(f"continuity: the daemon did not come up (state {daemon.state()})")
    since = time.strftime("%Y-%m-%d %H:%M:%S")
    failures = []

    # Capture runs realtime only once a stream exists, so check inside one.
    # rtkit answers through the portal a little after the stream starts.
    def check_realtime():
        for unit, what, loop in (
            (UNIT, "the daemon's capture", "pw-data-loop"),
            ("pipewire.service", "PipeWire", "data-loop.0"),
        ):
            pid = unit_pid(unit)
            deadline = time.monotonic() + 5
            while not (rt := [t for t in realtime(pid) if t[0] == loop]):
                if time.monotonic() > deadline:
                    failures.append(f"{what} is not realtime: {threads(pid)}")
                    break
                time.sleep(0.1)
            log(f"{what} realtime threads: {rt}")

    if f := dictate(daemon, 6, during=check_realtime):
        failures.append(f"realtime check: {f}")
    if failures:
        sys.exit("continuity: FAIL\n" + "\n".join(failures))

    cores = os.cpu_count()
    for label, load in (("idle", 0), (f"{2 * cores} busy loops on {cores} cpus", 2 * cores)):
        hogs = busy(load)
        try:
            for i in range(args.rounds):
                f = dictate(daemon, args.seconds)
                log(f"{label} dictation {i + 1}: {f or 'ok'}")
                if f:
                    failures.append(f"{label} dictation {i + 1}: {f}")
        finally:
            for h in hogs:
                h.kill()
    lost = losses(since)
    failures.extend(lost)

    # The positive control: a 150 ms freeze of the daemon must be reported.
    pid = unit_pid(UNIT)

    def freeze():
        time.sleep(2)
        os.kill(pid, signal.SIGSTOP)
        time.sleep(0.15)
        os.kill(pid, signal.SIGCONT)

    dictate(daemon, 3, during=freeze)
    if len(losses(since)) <= len(lost):
        failures.append("control: a 150 ms freeze of the daemon lost no audio, so nothing here can")
    daemon.wait(lambda s: s == "idle", 30)

    held.terminate()
    commits = [
        line for line in held.communicate(timeout=10)[0].splitlines() if line.startswith("commit")
    ]
    log(f"{len(commits)} commits into the field")
    if failures:
        sys.exit("continuity: FAIL\n" + "\n".join(failures))
    log(f"PASS: {2 * args.rounds} dictations of {args.seconds:g} s, no audio lost")


if __name__ == "__main__":
    main()
