#!/usr/bin/env python3
"""A GTK4 window of text fields for the dictation e2e: what the daemon injects into.

Run in the autologin user's real graphical session (GNOME Wayland, or Xfce on
X11), so text reaches it through the desktop's own input-method path. Three
fields: `plain` and `other` (ordinary entries) and `secret` (a password entry).

Every change is reported by rewriting a JSON state file atomically:

    {"plain": "...", "other": "...", "secret": "...",
     "focus": "plain", "active": true, "seq": 12}

`focus` is the focused field, `active` whether the compositor made the window
the active one. Suites read the file; they never look at pixels.

Control is the GApplication action `focus` (parameter: a field name) and
`clear`, reached over D-Bus:

    gdbus call --session --dest com.canonical.Myna.E2eField \\
        --object-path /com/canonical/Myna/E2eField \\
        --method org.freedesktop.Application.ActivateAction focus '[<"other">]' '{}'

Usage: field-app.py STATE_FILE
"""

import json
import os
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gio, GLib, Gtk  # noqa: E402

FIELDS = ("plain", "other", "secret")


class FieldApp(Gtk.Application):
    def __init__(self, state_path):
        super().__init__(application_id="com.canonical.Myna.E2eField")
        self.state_path = state_path
        self.seq = 0
        self.widgets = {}
        self.window = None

    def do_startup(self):
        Gtk.Application.do_startup(self)
        focus = Gio.SimpleAction.new("focus", GLib.VariantType.new("s"))
        focus.connect("activate", self.on_focus)
        self.add_action(focus)
        clear = Gio.SimpleAction.new("clear", None)
        clear.connect("activate", self.on_clear)
        self.add_action(clear)

    def do_activate(self):
        if self.window:
            self.window.present()
            return
        win = Gtk.ApplicationWindow(application=self, title="Myna e2e fields")
        win.set_default_size(480, 240)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        for margin in ("top", "bottom", "start", "end"):
            getattr(box, f"set_margin_{margin}")(16)
        for name in FIELDS:
            w = Gtk.PasswordEntry() if name == "secret" else Gtk.Entry()
            w.set_name(name)
            if name == "secret":
                w.set_show_peek_icon(False)
            # The editable inside the widget is the one that takes focus.
            inner = w.get_delegate()
            inner.connect("notify::has-focus", self.report)
            inner.connect("changed", self.report)
            self.widgets[name] = (w, inner)
            box.append(w)
        win.set_child(box)
        win.connect("notify::is-active", self.report)
        win.present()
        self.window = win
        self.widgets["plain"][1].grab_focus()
        self.report()

    def on_focus(self, _action, param):
        name = param.get_string()
        if name in self.widgets:
            self.widgets[name][1].grab_focus()
        self.report()

    def on_clear(self, *_):
        for _w, text in self.widgets.values():
            text.set_text("")
        self.report()

    def report(self, *_):
        self.seq += 1
        state = {name: text.get_text() for name, (_w, text) in self.widgets.items()}
        state["focus"] = next(
            (n for n, (_w, text) in self.widgets.items() if text.has_focus()), None
        )
        state["active"] = bool(self.window and self.window.is_active())
        state["seq"] = self.seq
        tmp = self.state_path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(state, f)
        os.replace(tmp, self.state_path)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    sys.exit(FieldApp(sys.argv[1]).run([sys.argv[0]]))
