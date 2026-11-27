#!/usr/bin/env python3
"""A GTK4 text view on text-input-v3 that mirrors its buffer to a file.

usage: field.py OUT
"""

import os
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402

OUT = sys.argv[1]


def changed(buffer):
    start, end = buffer.get_bounds()
    with open(OUT + ".tmp", "w") as tmp:
        tmp.write(buffer.get_text(start, end, True))
    os.replace(OUT + ".tmp", OUT)


def activate(app):
    window = Gtk.ApplicationWindow(application=app, default_width=800, default_height=600)
    view = Gtk.TextView()
    view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
    view.get_buffer().connect("changed", changed)
    scrolled = Gtk.ScrolledWindow()
    scrolled.set_child(view)
    window.set_child(scrolled)
    window.present()
    view.grab_focus()
    with open(OUT, "w"):
        pass


app = Gtk.Application(application_id="com.example.RigField")
app.connect("activate", activate)
app.run([])
