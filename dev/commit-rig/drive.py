#!/usr/bin/env python3
"""The scenarios of dev/commit-rig/run.sh, which runs this in the nested session.

Each argument is evaluated with sizes, burst, stalled, Gtk4 and Noo in scope.
The text is numbered words, generated here as the probe generates it, so what
landed is compared with what was sent.
"""

import os
import signal
import subprocess
import sys
import threading
import time

from gi.repository import Gio, GLib

OUT = os.environ["OUT"]
RT = os.environ["XDG_RUNTIME_DIR"]
RIG = os.path.dirname(os.path.abspath(__file__))
PROBE = os.environ["MYNA"] + "/client/target/debug/examples/commit_probe"
NOO = "/snap/noosphere/current/bin/noo"
NOOSD = "/snap/noosphere/current/bin/noosd"
REMOTE = "org.gnome.Mutter.RemoteDesktop"
LEFT_SHIFT = 42

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def proxy(name, path, iface):
    return Gio.DBusProxy.new_sync(bus, 0, None, name, path, iface)


def remote_session():
    """The Shell claims RemoteDesktop some time after its socket appears."""
    dbus = proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")
    end = time.monotonic() + 30
    owned = GLib.Variant("(s)", (REMOTE,))
    while not dbus.call_sync("NameHasOwner", owned, 0, -1, None).unpack()[0]:
        if time.monotonic() > end:
            sys.exit("commit-rig: the Shell never provided RemoteDesktop")
        time.sleep(0.05)
    remote = proxy(REMOTE, "/org/gnome/Mutter/RemoteDesktop", REMOTE)
    path = remote.call_sync("CreateSession", None, 0, -1, None).unpack()[0]
    session = proxy(REMOTE, path, REMOTE + ".Session")
    session.call_sync("Start", None, 0, -1, None)
    return session


session = remote_session()


def call(method, signature, *args):
    session.call_sync(method, GLib.Variant(signature, args), 0, -1, None)
    time.sleep(0.02)


def focus():
    """Pointer onto the window, and a key: Mutter makes its keyboard on the first."""
    call("NotifyPointerMotionRelative", "(dd)", -16000.0, -10000.0)
    time.sleep(0.1)
    call("NotifyPointerMotionRelative", "(dd)", 800.0, 500.0)
    call("NotifyKeyboardKeycode", "(ub)", LEFT_SHIFT, True)
    call("NotifyKeyboardKeycode", "(ub)", LEFT_SHIFT, False)
    time.sleep(0.5)


def wait_until(what, done, timeout=15):
    end = time.monotonic() + timeout
    while not done():
        if time.monotonic() > end:
            sys.exit(f"commit-rig: no {what} within {timeout} s")
        time.sleep(0.05)


class Probe:
    def __init__(self):
        self.process = subprocess.Popen(
            [PROBE],
            env=dict(os.environ, MYNA_DEBUG="1"),
            stderr=open(f"{OUT}/probe.err", "w"),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.word = 0

    def say(self, line, replies=1):
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()
        return [self.process.stdout.readline().strip() for _ in range(replies)]

    def text(self, size):
        out = ""
        while len(out) < size:
            out += f"{self.word % 100000:05d} "
            self.word += 1
        return out[:size]

    def commit(self, size):
        text = self.text(size)
        return text, self.say(f"commit {size}")[0]

    def burst(self, count, size, gap):
        texts = [self.text(size) for _ in range(count)]
        self.say(f"burst {count} {size} {gap}", count)
        return texts


probe = Probe()


class Field:
    def alive(self):
        return self.process.poll() is None

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(5)
        except subprocess.TimeoutExpired:
            self.process.kill()


class Gtk4(Field):
    name = "gtk4"

    def __init__(self, tag):
        self.path = f"{OUT}/{tag}.txt"
        self.log = open(f"{OUT}/{tag}.wl", "w")
        self.process = subprocess.Popen(
            ["python3", f"{RIG}/field.py", self.path],
            env=dict(os.environ, WAYLAND_DEBUG="1"),
            stderr=self.log,
        )
        wait_until("field", lambda: os.path.exists(self.path))
        time.sleep(0.5)
        focus()

    def got(self):
        with open(self.path) as file:
            return file.read()

    def said(self):
        """Why the field died, if it did."""
        self.log.flush()
        with open(self.log.name, errors="replace") as file:
            return [line.strip() for line in file if "Error reading events" in line]


class Noo(Field):
    name = "noo"

    def __init__(self, tag):
        self.path = f"{OUT}/{tag}.bytes"
        self.trace = open(f"{OUT}/{tag}.trace", "w")
        echo = f"{RT}/echo.py"
        with open(echo, "w") as file:
            file.write(
                "import os, tty\n"
                "tty.setraw(0)\n"
                f'f = open({self.path!r}, "ab", 0)\n'
                'os.write(1, b"IME-READY\\r\\n> ")\n'
                "while True:\n"
                "    b = os.read(0, 65536)\n"
                "    f.write(b)\n"
                "    os.write(1, b)\n"
            )
        open(self.path, "w").close()
        wait_until("noosd", lambda: os.path.exists(f"{RT}/noosphere/default.sock"))
        spawn = [NOOSD, "ctl", "spawn", "--rows", "24", "--cols", "80", "--"]
        pane = subprocess.run(
            [*spawn, "python3", "-u", echo],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
        wait = [NOOSD, "ctl", "wait", "--pane", pane, "--pattern", "IME-READY"]
        subprocess.run(["timeout", "10", *wait], check=True)
        self.process = subprocess.Popen(
            [NOO], env=dict(os.environ, NOO_TRACE="1"), stderr=self.trace
        )
        wait_until("noo frame", lambda: "frame" in open(self.trace.name).read())
        focus()

    def got(self):
        with open(self.path, "rb") as file:
            return file.read().decode(errors="replace")

    def said(self):
        return []


def report(label, field, sent, settle=1.0):
    """Print how many of the texts `sent`, in order, are in the field."""
    time.sleep(settle)
    got = field.got()
    at = 0
    lost = 0
    for text in sent:
        found = got.find(text, at)
        if found < 0:
            lost += 1
        else:
            at = found + len(text)
    print(
        f"{label}: sent {len(sent)} x {len(sent[0].encode())} bytes, "
        f"landed {len(got.encode())} bytes, lost {lost} of {len(sent)}, "
        f"field alive: {field.alive()}",
        flush=True,
    )
    for line in field.said():
        print("   ", line[:160], flush=True)


def finish(field):
    try:
        probe.say("release")
    except OSError as error:
        print("    release failed:", error)
    field.close()


def sizes(kind, tag, each):
    """One commit of each size in bytes, into a fresh field."""
    for size in each:
        field = kind(f"{tag}-{size}")
        probe.say("acquire")
        text, result = probe.commit(size)
        report(f"{field.name} one commit ({result.split()[-1]})", field, [text])
        finish(field)


def burst(kind, tag, count, size, gaps):
    """`count` commits of `size` bytes, `gap` ms apart, for each gap."""
    for gap in gaps:
        field = kind(f"{tag}-gap{gap}")
        probe.say("acquire")
        texts = probe.burst(count, size, gap)
        report(f"{field.name} {gap} ms apart", field, texts)
        finish(field)


def nested_shell():
    """The Shell of this session, never the desktop's."""
    pids = subprocess.run(["pgrep", "-x", "gnome-shell"], capture_output=True, text=True)
    ours = f"XDG_RUNTIME_DIR={RT}".encode()
    for pid in pids.stdout.split():
        try:
            with open(f"/proc/{pid}/environ", "rb") as file:
                if ours in file.read().split(b"\0"):
                    return int(pid)
        except OSError:
            pass
    sys.exit("commit-rig: no nested Shell")


def stalled(kind, tag, count, size, gap, stall, repeats=1):
    """`burst`, `repeats` times, the Shell stopped `stall` ms as each starts."""
    shell = nested_shell()
    field = kind(f"{tag}-gap{gap}-stall{stall}")
    probe.say("acquire")
    sent = []
    for _ in range(repeats):
        if stall:
            os.kill(shell, signal.SIGSTOP)
            resume = threading.Timer(stall / 1000, os.kill, (shell, signal.SIGCONT))
            resume.start()
        sent += probe.burst(count, size, gap)
        time.sleep(stall / 1000 + 0.3)
    report(f"{field.name} {gap} ms apart, Shell stalled {stall} ms", field, sent)
    finish(field)


SCENARIOS = {"sizes": sizes, "burst": burst, "stalled": stalled, "Gtk4": Gtk4, "Noo": Noo}
for scenario in sys.argv[1:]:
    eval(scenario, SCENARIOS)  # noqa: S307
probe.process.stdin.close()
