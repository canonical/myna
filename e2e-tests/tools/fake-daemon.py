#!/usr/bin/python3
"""A stand-in dictation daemon on a private session bus (shot.sh --fake-daemon).

Serves com.canonical.Myna.Dictation with Activation "portal" and a Shortcut
driven by files in the working directory, so the shortcut step's bound and
live-changed states can be shot on stonking without the portal's dialog ever
reaching Charles's live desktop:

  fake-bind      "grant <accel>" (BindShortcut grants it after 1.5 s, the
                 dialog's time) or "dismiss" (the default: the dialog closed)
  fake-shortcut  its content becomes the Shortcut ("Press <content>") when it
                 changes: a rebind elsewhere, e.g. in GNOME Settings
  fake-restart   touched: drop the bus connection and come back on a new one,
                 as the daemon does when setup restarts it

It also owns org.freedesktop.portal.Desktop, forwarding the Settings calls to
the real session's portal (REAL_BUS), so the app keeps the live accent and
colour scheme.
"""

import os

from gi.repository import Gio, GLib

NAME = "com.canonical.Myna.Dictation"
PATH = "/com/canonical/Myna/Dictation"
XML = """<node>
<interface name="com.canonical.Myna.Dictation">
  <method name="BindShortcut"><arg type="s" direction="in"/>
    <arg type="b" direction="out"/><arg type="s" direction="out"/></method>
  <method name="Toggle"/>
  <property name="Activation" type="s" access="read"/>
  <property name="Shortcut" type="s" access="read"/>
  <property name="State" type="s" access="read"/>
  <property name="HudStyle" type="s" access="read"/>
  <property name="StatusMessage" type="s" access="read"/>
</interface>
<interface name="org.freedesktop.portal.Settings">
  <method name="ReadAll"><arg type="as" direction="in"/>
    <arg type="a{sa{sv}}" direction="out"/></method>
  <method name="Read"><arg type="s" direction="in"/><arg type="s" direction="in"/>
    <arg type="v" direction="out"/></method>
  <method name="ReadOne"><arg type="s" direction="in"/><arg type="s" direction="in"/>
    <arg type="v" direction="out"/></method>
  <property name="version" type="u" access="read"/>
  <signal name="SettingChanged"><arg type="s"/><arg type="s"/><arg type="v"/></signal>
</interface>
</node>"""
INFO = Gio.DBusNodeInfo.new_for_xml(XML)
real = Gio.DBusConnection.new_for_address_sync(
    os.environ["REAL_BUS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None,
    None,
)
state = {"shortcut": "", "seen": None, "conn": None, "ids": []}


def read(name):
    try:
        with open(name) as f:
            return f.read().strip()
    except OSError:
        return None


def announce():
    conn = state["conn"]
    changed = {"Shortcut": GLib.Variant("s", state["shortcut"])}
    conn.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                     GLib.Variant("(sa{sv}as)", (NAME, changed, [])))


def dictation_call(conn, sender, path, iface, method, params, invocation):
    if method == "BindShortcut":
        answer = read("fake-bind") or "dismiss"

        def reply():
            if answer.startswith("grant "):
                state["shortcut"] = "Press " + answer.split(" ", 1)[1]
                announce()
                invocation.return_value(GLib.Variant("(bs)", (True, "")))
            else:
                invocation.return_value(GLib.Variant("(bs)", (False, "the dialog was dismissed")))
            return False

        GLib.timeout_add(1500, reply)
    else:
        invocation.return_value(None)


def dictation_get(conn, sender, path, iface, prop):
    return GLib.Variant("s", {
        "Activation": "portal",
        "Shortcut": state["shortcut"],
        "State": "idle",
        "HudStyle": "bar",
        "StatusMessage": "",
    }[prop])


def settings_call(conn, sender, path, iface, method, params, invocation):
    try:
        out = real.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
                             iface, method, params, None, Gio.DBusCallFlags.NONE, 5000, None)
        invocation.return_value(out)
    except GLib.Error as error:
        invocation.return_gerror(error)


def settings_get(conn, sender, path, iface, prop):
    return GLib.Variant("u", 2)


def connect():
    conn = Gio.DBusConnection.new_for_address_sync(
        os.environ["DBUS_SESSION_BUS_ADDRESS"],
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None,
        None,
    )
    conn.set_exit_on_close(False)
    conn.register_object(PATH, INFO.interfaces[0], dictation_call, dictation_get, None)
    conn.register_object("/org/freedesktop/portal/desktop", INFO.interfaces[1],
                         settings_call, settings_get, None)
    for name in (NAME, "org.freedesktop.portal.Desktop"):
        conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                       "RequestName", GLib.Variant("(su)", (name, 4)), None,
                       Gio.DBusCallFlags.NONE, -1, None)
    state["conn"] = conn
    print("fake-daemon: on the bus as", conn.get_unique_name(), flush=True)


def tick():
    if os.path.exists("fake-restart"):
        os.remove("fake-restart")
        state["conn"].close_sync(None)
        connect()
    wanted = read("fake-shortcut")
    if wanted is not None and wanted != state["seen"]:
        state["seen"] = wanted
        state["shortcut"] = "Press " + wanted if wanted else ""
        announce()
    return True


for stale in ("fake-restart", "fake-shortcut"):
    if os.path.exists(stale):
        os.remove(stale)
connect()
GLib.timeout_add(200, tick)
GLib.MainLoop().run()
