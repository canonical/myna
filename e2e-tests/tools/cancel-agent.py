#!/usr/bin/python3
"""A polkit authentication agent for one process that dismisses every prompt.

Usage: cancel-agent.py PID [DELAY_SECONDS]

Answers BeginAuthentication with org.freedesktop.PolicyKit1.Error.Cancelled,
which polkitd records as the user dismissing the dialog; snapd then fails the
request with kind "auth-cancelled", exactly as when a user presses Cancel.
"""

import sys

from gi.repository import Gio, GLib

PID = int(sys.argv[1])
DELAY = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
PATH = "/com/canonical/Myna/ShotCancelAgent"

XML = """
<node>
  <interface name="org.freedesktop.PolicyKit1.AuthenticationAgent">
    <method name="BeginAuthentication">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/><arg type="a{ss}" direction="in"/>
      <arg type="s" direction="in"/><arg type="a(sa{sv})" direction="in"/>
    </method>
    <method name="CancelAuthentication"><arg type="s" direction="in"/></method>
  </interface>
</node>
"""


def start_time(pid):
    with open(f"/proc/{pid}/stat") as f:
        return int(f.read().rsplit(")", 1)[1].split()[19])


def on_call(conn, sender, path, iface, method, params, invocation):
    if method == "BeginAuthentication":
        print(f"cancel-agent: dismissing {params[0]}", flush=True)

        def dismiss():
            invocation.return_dbus_error(
                "org.freedesktop.PolicyKit1.Error.Cancelled", "dismissed by noble-shot"
            )
            return False

        GLib.timeout_add(int(DELAY * 1000), dismiss)
    else:
        invocation.return_value(None)


bus = Gio.bus_get_sync(Gio.BusType.SYSTEM)
node = Gio.DBusNodeInfo.new_for_xml(XML)
bus.register_object(PATH, node.interfaces[0], on_call, None, None)
subject = (
    "unix-process",
    {"pid": GLib.Variant("u", PID), "start-time": GLib.Variant("t", start_time(PID))},
)
bus.call_sync(
    "org.freedesktop.PolicyKit1",
    "/org/freedesktop/PolicyKit1/Authority",
    "org.freedesktop.PolicyKit1.Authority",
    "RegisterAuthenticationAgent",
    GLib.Variant("((sa{sv})ss)", (subject, "en_US.UTF-8", PATH)),
    None, Gio.DBusCallFlags.NONE, -1, None,
)
print(f"cancel-agent: registered for pid {PID}", flush=True)
GLib.MainLoop().run()
