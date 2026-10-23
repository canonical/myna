#!/usr/bin/env python3
"""A window of text fields for the dictation e2e: what the daemon injects into.

Run in the autologin user's real graphical session, so text reaches it through
the desktop's own input-method path. Three fields: `plain` and `other`
(ordinary entries) and `secret` (a password entry), in one GTK4 window by
default. `--qt` makes them Qt6 widgets instead, the toolkit of Plasma's own
apps, which reaches KWin through text-input-v2 where GTK4 speaks v3. There
`other` has a window of its own: KWin passes a v2 focus move inside a window
on to the input method as a mere state update, so IBus cannot tell the field
changed (an upstream bug; see e2e-tests/AGENTS.md).

Every change is reported by rewriting a JSON state file atomically:

    {"plain": "...", "other": "...", "secret": "...",
     "focus": "plain", "active": true, "seq": 12}

`focus` is the focused field, `active` whether the compositor made its window
the active one. Suites read the file; they never look at pixels.

Control is the GApplication action `focus` (parameter: a field name) and
`clear`, reached over D-Bus, whichever the toolkit:

    gdbus call --session --dest com.canonical.Myna.E2eField \\
        --object-path /com/canonical/Myna/E2eField \\
        --method org.freedesktop.Application.ActivateAction focus '[<"other">]' '{}'

Usage: field-app.py [--qt] STATE_FILE
"""

import json
import os
import sys

import gi
from gi.repository import Gio, GLib

APP_ID = "com.canonical.Myna.E2eField"
TITLE = "Myna e2e fields"
FIELDS = ("plain", "other", "secret")


class State:
    def __init__(self, path):
        self.path = path
        self.seq = 0

    def write(self, texts, focus, active):
        self.seq += 1
        state = dict(texts, focus=focus, active=bool(active), seq=self.seq)
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(state, f)
        os.replace(tmp, self.path)


def add_controls(app, focus, clear):
    action = Gio.SimpleAction.new("focus", GLib.VariantType.new("s"))
    action.connect("activate", lambda _a, param: focus(param.get_string()))
    app.add_action(action)
    action = Gio.SimpleAction.new("clear", None)
    action.connect("activate", lambda *_: clear())
    app.add_action(action)


def run_gtk(state):
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk

    app = Gtk.Application(application_id=APP_ID)
    fields = {}
    window = None

    def report(*_):
        state.write(
            {name: text.get_text() for name, text in fields.items()},
            next((n for n, text in fields.items() if text.has_focus()), None),
            window and window.is_active(),
        )

    def focus(name):
        if name in fields:
            fields[name].grab_focus()
        report()

    def clear():
        for text in fields.values():
            text.set_text("")
        report()

    def activate(_app):
        nonlocal window
        if window:
            window.present()
            return
        window = Gtk.ApplicationWindow(application=app, title=TITLE)
        window.set_default_size(480, 240)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        for margin in ("top", "bottom", "start", "end"):
            getattr(box, f"set_margin_{margin}")(16)
        for name in FIELDS:
            w = Gtk.PasswordEntry() if name == "secret" else Gtk.Entry()
            if name == "secret":
                w.set_show_peek_icon(False)
            # The editable inside the widget is the one that takes focus.
            inner = w.get_delegate()
            inner.connect("notify::has-focus", report)
            inner.connect("changed", report)
            fields[name] = inner
            box.append(w)
        window.set_child(box)
        window.connect("notify::is-active", report)
        window.present()
        fields["plain"].grab_focus()
        report()

    app.connect("startup", lambda _app: add_controls(app, focus, clear))
    app.connect("activate", activate)
    return app.run([sys.argv[0]])


def run_qt(state):
    from PyQt6.QtWidgets import QApplication, QFormLayout, QLineEdit, QWidget

    qt = QApplication([sys.argv[0]])
    windows = {TITLE: QWidget(), f"{TITLE} (other)": QWidget()}
    forms = [QFormLayout(window) for window in windows.values()]
    fields = {}

    def report(*_):
        focused = qt.focusWidget()
        state.write(
            {name: w.text() for name, w in fields.items()},
            next((n for n, w in fields.items() if w is focused), None),
            qt.activeWindow() is not None,
        )

    def focus(name):
        fields[name].window().activateWindow()
        fields[name].setFocus()

    for name in FIELDS:
        w = QLineEdit()
        if name == "secret":
            w.setEchoMode(QLineEdit.EchoMode.Password)
        w.textChanged.connect(report)
        forms[name == "other"].addRow(name, w)
        fields[name] = w

    # Qt's event loop runs GLib's default context, so a bare GApplication
    # serves the same D-Bus controls as the GTK window's.
    control = Gio.Application(application_id=APP_ID)
    add_controls(control, focus, lambda: [w.clear() for w in fields.values()])
    control.register()
    qt.focusChanged.connect(report)
    qt.applicationStateChanged.connect(report)
    for title, window in reversed(windows.items()):
        window.setWindowTitle(title)
        window.resize(480, 120)
        window.show()
    focus("plain")
    report()
    return qt.exec()


if __name__ == "__main__":
    args = sys.argv[1:]
    qt = args[:1] == ["--qt"]
    if len(args) != 1 + qt:
        sys.exit(__doc__)
    state = State(args[-1])
    sys.exit(run_qt(state) if qt else run_gtk(state))
