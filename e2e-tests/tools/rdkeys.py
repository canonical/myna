#!/usr/bin/python3
"""Type key chords into the VM's real GNOME Wayland session through
Mutter's RemoteDesktop API (no xdotool there).

  rdkeys.py [--tab-to APP NAME[@ROLE]] CHORD...
  CHORD is keysyms joined by "+", e.g. Return, Super_L+k, Tab.
--tab-to first presses Tab (at most 60 times) until the widget with that
name in the app whose name contains APP reports FOCUSED; GTK 4 does not
implement AT-SPI's GrabFocus.
"""
import sys
import time

import gi

gi.require_version("Gdk", "4.0")

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gdk, Gio, GLib  # noqa: E402


def target_focused(app, target):
    name, _, role = target.rpartition("@") if "@" in target else (target, "", "")
    desktop = Atspi.get_desktop(0)

    def walk(node):
        yield node
        try:
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child is not None:
                    yield from walk(child)
        except Exception:
            return

    for i in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(i)
        if child is None or app not in (child.get_name() or ""):
            continue
        for node in walk(child):
            try:
                if (node.get_name() == name and (not role or node.get_role_name() == role)
                        and node.get_state_set().contains(Atspi.StateType.FOCUSED)):
                    return True
            except Exception:
                continue
    return False


args = sys.argv[1:]
tab_to = None
if args[:1] == ["--tab-to"]:
    tab_to = (args[1], args[2])
    args = args[3:]

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
path = bus.call_sync("org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop",
                     "org.gnome.Mutter.RemoteDesktop", "CreateSession", None,
                     GLib.VariantType("(o)"), 0, 5000).unpack()[0]


def session(method, params=None):
    bus.call_sync("org.gnome.Mutter.RemoteDesktop", path,
                  "org.gnome.Mutter.RemoteDesktop.Session", method, params, None, 0, 5000)


session("Start")
time.sleep(0.2)


def press(syms):
    for sym in syms:
        session("NotifyKeyboardKeysym", GLib.Variant("(ub)", (sym, True)))
        time.sleep(0.05)
    for sym in reversed(syms):
        session("NotifyKeyboardKeysym", GLib.Variant("(ub)", (sym, False)))
        time.sleep(0.05)


if tab_to:
    for _ in range(60):
        if target_focused(*tab_to):
            break
        press([Gdk.keyval_from_name("Tab")])
        time.sleep(0.25)
    else:
        session("Stop")
        sys.exit(f"rdkeys: Tab never reached {tab_to[1]!r}")
for chord in args:
    press([Gdk.keyval_from_name(k) for k in chord.split("+")])
    time.sleep(0.4)
session("Stop")
print("rdkeys: typed", " ".join(args))
