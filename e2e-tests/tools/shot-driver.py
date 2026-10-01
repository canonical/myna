#!/usr/bin/python3
"""Drive a running myna-config under Xvfb through AT-SPI, take screenshots.

Runs on the Noble box inside xvfb-run (DISPLAY set). Steps are argv words:

  tab:<label>          click the view-switcher tab with that label
  click:<name>         real X click on the first showing widget with that
                       accessible name (xdotool at its extents); falls back to
                       the AT-SPI action when extents are unknown
  activate:<name>      AT-SPI action only (no pointer)
  waitfor:<name>       wait (<=30 s) until a showing widget has that name
  gone:<name>          wait (<=60 s) until no showing widget has that name
  wait:<seconds>       sleep
  key:<keysym>         xdotool key (e.g. Escape, ctrl+w)
  shot:<file.png>      screenshot the screen (= the window) to OUT_DIR
  shotwin:<file.png>   screenshot only the newest app window (use with a leading
                       "natural" step, which keeps its default size)
  burst:<prefix>:<interval>:<max_s>[:<name>]
                       screenshot <prefix>-NNN.png every <interval> s, for at
                       most <max_s> s or until a widget named <name> shows
  dump                 print the showing accessible tree (name/role) to stdout
  blur                 open a stand-in window (xmessage) and give it focus
  refocus              give the app's window focus again, close the stand-in
  sh:<command>         run a shell command on the box (e.g. sudo snap ...)
  front[:W,H]          move the app's newest window (a modal wizard) to 0,0
                       and raise it, optionally resizing it

Names match exactly first, then as a case-insensitive substring. Any name
may carry a role filter, "name@role" (e.g. "Backend@combo box"); `dump`
prints the roles.
"""

import os
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

OUT_DIR = os.environ.get("OUT_DIR", os.path.expanduser("~/myna-shot/out"))
APP_NAMES = ("myna-config", "Myna Settings", "com.canonical.Myna.Config")
PID = int(os.environ["APP_PID"])


def app():
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(i)
        if child is None:
            continue
        try:
            if child.get_process_id() == PID or child.get_name() in APP_NAMES:
                return child
        except gi.repository.GLib.Error:
            continue
    return None


def showing(node):
    try:
        return node.get_state_set().contains(Atspi.StateType.SHOWING)
    except Exception:
        return False


def walk(node, depth=0):
    yield node, depth
    try:
        count = node.get_child_count()
    except Exception:
        return
    for i in range(count):
        child = node.get_child_at_index(i)
        if child is not None and showing(child):
            yield from walk(child, depth + 1)


def walk_all(node):
    # Another toolkit's tree can hide SHOWING on a container of showing widgets.
    yield node
    try:
        count = node.get_child_count()
    except Exception:
        return
    for i in range(count):
        child = node.get_child_at_index(i)
        if child is not None:
            yield from walk_all(child)


def name_of(node):
    try:
        return node.get_name() or ""
    except Exception:
        return ""


def find(name, roles=None):
    root = app()
    if root is None:
        return None
    nodes = [n for n, _ in walk(root) if showing(n)]
    if roles:
        # A node can vanish mid-walk when the app rebuilds that part.
        def role_of(n):
            try:
                return n.get_role_name()
            except Exception:
                return None
        nodes = [n for n in nodes if role_of(n) in roles]
    for n in nodes:
        if name_of(n) == name:
            return n
    low = name.lower()
    for n in nodes:
        if low in name_of(n).lower():
            return n
    return None


def wait_for(name, timeout=30.0, roles=None):
    if "@" in name:
        name, _, role = name.rpartition("@")
        roles = (role,)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node = find(name, roles)
        if node is not None:
            return node
        time.sleep(0.2)
    sys.exit(f"shot-driver: no showing widget named {name!r} after {timeout}s")


def window_origin():
    wid = subprocess.run(
        ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(PID)],
        capture_output=True, text=True, check=True,
    ).stdout.split()[0]
    geo = subprocess.run(
        ["xdotool", "getwindowgeometry", "--shell", wid],
        capture_output=True, text=True, check=True,
    ).stdout
    vals = dict(line.split("=", 1) for line in geo.split())
    return int(vals["X"]), int(vals["Y"])


def x_click(node):
    try:
        ext = node.get_extents(Atspi.CoordType.WINDOW)
    except Exception:
        ext = None
    if ext is None or ext.width <= 0 or ext.height <= 0:
        return False
    ox, oy = window_origin()
    x, y = ox + ext.x + ext.width // 2, oy + ext.y + ext.height // 2
    subprocess.run(["xdotool", "mousemove", "--sync", str(x), str(y), "click", "1"], check=True)
    return True


def do_action(node):
    try:
        for i in range(node.get_n_actions()):
            if node.get_action_name(i) in ("click", "activate", "press", "toggle"):
                return node.do_action(i)
        if node.get_n_actions():
            return node.do_action(0)
    except Exception:
        pass
    return False


STAND_IN = None


def app_window():
    return subprocess.run(
        ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(PID)],
        capture_output=True, text=True, check=True,
    ).stdout.split()[0]


def blur():
    global STAND_IN
    STAND_IN = subprocess.Popen(
        ["xmessage", "-geometry", "360x60+700+700", "App Center stand-in"])
    wid = subprocess.run(
        ["timeout", "10", "xdotool", "search", "--sync", "--onlyvisible", "--name", "^xmessage$"],
        capture_output=True, text=True, check=True,
    ).stdout.split()[0]
    subprocess.run(["xdotool", "windowfocus", "--sync", wid], check=True)


def refocus():
    global STAND_IN
    subprocess.run(["xdotool", "windowfocus", "--sync", app_window()], check=True)
    if STAND_IN is not None:
        STAND_IN.kill()
        STAND_IN.wait()
        STAND_IN = None


def dump():
    root = app()
    for n, depth in walk(root):
        description = n.get_description() or ""
        suffix = f" ({description!r})" if description else ""
        print("  " * depth + f"[{n.get_role_name()}] {name_of(n)!r}{suffix}")


def main(steps):
    os.makedirs(OUT_DIR, exist_ok=True)
    deadline = time.monotonic() + 30
    while app() is None:
        if time.monotonic() > deadline:
            sys.exit("shot-driver: myna-config never appeared on the a11y bus")
        time.sleep(0.2)
    for step in steps:
        verb, _, arg = step.partition(":")
        print(f"shot-driver: {step}", flush=True)
        if verb == "tab":
            node = wait_for(arg, roles=("toggle button", "button", "page tab", "radio button"))
            x_click(node) or do_action(node)
        elif verb == "click":
            node = wait_for(arg)
            x_click(node) or do_action(node)
        elif verb == "scrollat":
            # scrollat:<x>,<y>[#clicks]: wheel down at root coordinates
            # (AT-SPI extents inside a popover are relative to the popover)
            # (a negative count scrolls up)
            at, _, clicks = arg.partition("#")
            x, y = at.split(",")
            count = int(clicks or "10")
            subprocess.run(["xdotool", "mousemove", "--sync", x, y,
                            "click", "--repeat", str(abs(count)), "--delay", "30",
                            "4" if count < 0 else "5"], check=True)
        elif verb == "activate":
            do_action(wait_for(arg))
        elif verb == "waitfor":
            wait_for(arg)
        elif verb == "gone":
            deadline = time.monotonic() + 60
            owner, _, arg = arg.rpartition("/")
            name, _, role = arg.rpartition("@") if "@" in arg else (arg, "", "")
            while find(name, (role,) if role else None) is not None:
                if time.monotonic() > deadline:
                    sys.exit(f"shot-driver: {arg!r} still showing after 60s")
                time.sleep(0.2)
        elif verb == "wait":
            time.sleep(float(arg))
        elif verb == "key":
            subprocess.run(["xdotool", "key", arg], check=True)
        elif verb == "type":
            subprocess.run(["xdotool", "type", "--delay", "60", arg], check=True)
        elif verb == "shot":
            time.sleep(0.4)  # let the frame after the last event paint
            path = os.path.join(OUT_DIR, arg)
            subprocess.run(["import", "-window", "root", path], check=True)
            print(f"shot-driver: wrote {path}", flush=True)
        elif verb == "shotwin":
            # shotwin:NAME: the app's newest window only, cropped from the root.
            time.sleep(0.4)
            wid = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(PID)],
                                 capture_output=True, text=True).stdout.split()[-1]
            geo = dict(line.split("=", 1) for line in subprocess.run(
                ["xdotool", "getwindowgeometry", "--shell", wid],
                capture_output=True, text=True, check=True).stdout.split())
            path = os.path.join(OUT_DIR, arg)
            subprocess.run(["import", "-window", "root", "-crop",
                            f"{geo['WIDTH']}x{geo['HEIGHT']}+{geo['X']}+{geo['Y']}", "+repage", path],
                           check=True)
            print(f"shot-driver: wrote {path} ({geo['WIDTH']}x{geo['HEIGHT']})", flush=True)
        elif verb == "burst":
            prefix, interval, limit, *stop = arg.split(":", 3)
            start = time.monotonic()
            n = 0
            while True:
                path = os.path.join(OUT_DIR, f"{prefix}-{n:03d}.png")
                subprocess.run(["import", "-window", "root", path], check=True)
                print(f"shot-driver: {time.strftime('%T')} wrote {path}", flush=True)
                n += 1
                if stop and find(stop[0]) is not None:
                    time.sleep(0.4)
                    subprocess.run(["import", "-window", "root",
                                    os.path.join(OUT_DIR, f"{prefix}-{n:03d}.png")], check=True)
                    break
                if time.monotonic() - start > float(limit):
                    break
                time.sleep(float(interval))
        elif verb == "front":
            # front[:W,H]: move the newest visible window of the app (the
            # wizard opened modal from the menu) to 0,0, optionally resize it.
            wids = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(PID)],
                                  capture_output=True, text=True).stdout.split()
            if not wids:
                sys.exit("shot-driver: no visible window to bring to the front")
            command = ["xdotool", "windowmove", wids[-1], "0", "0"]
            if arg:
                command += ["windowsize", wids[-1], *arg.split(",")]
            subprocess.run(command + ["windowactivate", wids[-1]], check=False)
        elif verb == "realshot":
            # realshot:FILE: the host's real session through the Screenshot
            # portal (resolute, noble); never on stonking, Charles's desktop.
            if os.environ.get("SHOT_HOST") == "stonking":
                sys.exit("shot-driver: realshot refused on stonking")
            env = {k: v for k, v in os.environ.items() if k != "DISPLAY"}
            subprocess.run(["python3", "realshot.py", os.path.join(OUT_DIR, arg)], env=env, check=True)
        elif verb == "other":
            # other:[APP/]NAME[@role]: AT-SPI action on a widget of another app
            # (whose a11y name contains APP) on
            # the real session (the portal's dialog); never on stonking.
            if os.environ.get("SHOT_HOST") == "stonking":
                sys.exit("shot-driver: other refused on stonking")
            owner, _, arg = arg.rpartition("/")
            name, _, role = arg.rpartition("@") if "@" in arg else (arg, "", "")
            deadline = time.monotonic() + 30
            hit = None
            while hit is None and time.monotonic() < deadline:
                desktop = Atspi.get_desktop(0)
                for i in range(desktop.get_child_count()):
                    child = desktop.get_child_at_index(i)
                    try:
                        if child is None or child.get_process_id() == PID:
                            continue
                        if owner and owner not in name_of(child):
                            continue
                    except Exception:
                        continue
                    for n in walk_all(child):
                        try:
                            if showing(n) and name_of(n) == name and (not role or n.get_role_name() == role):
                                hit = n
                                break
                        except Exception:
                            continue
                    if hit:
                        break
                if hit is None:
                    time.sleep(0.5)
            if hit is None:
                sys.exit(f"shot-driver: no other app shows {arg!r}")
            do_action(hit)
        elif verb == "otherdump":
            desktop = Atspi.get_desktop(0)
            for i in range(desktop.get_child_count()):
                child = desktop.get_child_at_index(i)
                if child is None:
                    continue
                print(f"app {name_of(child)!r} pid {child.get_process_id()}")
                if arg not in name_of(child):
                    continue
                for n, depth in walk(child):
                    print("  " * depth + f"[{n.get_role_name()}] {name_of(n)!r}")
        elif verb == "dump":
            dump()
        elif verb == "blur":
            blur()
        elif verb == "refocus":
            refocus()
        elif verb == "sh":
            result = subprocess.run(arg, shell=True, capture_output=True, text=True)
            print(result.stdout + result.stderr, end="", flush=True)
            if result.returncode:
                sys.exit(f"shot-driver: {arg!r} exited {result.returncode}")
        else:
            sys.exit(f"shot-driver: unknown step {step!r}")


if __name__ == "__main__":
    main(sys.argv[1:])
