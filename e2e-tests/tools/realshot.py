#!/usr/bin/python3
"""Screenshot the real GNOME session through the Screenshot portal, non-interactively."""
import shutil, sys, urllib.parse
from gi.repository import Gio, GLib
out = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
try:
    bus.call_sync("org.freedesktop.impl.portal.PermissionStore", "/org/freedesktop/impl/portal/PermissionStore",
        "org.freedesktop.impl.portal.PermissionStore", "SetPermission",
        GLib.Variant("(sbssas)", ("screenshot", True, "screenshot", "", ["yes"])), None, 0, 5000, None)
except GLib.Error as e:
    print("permission:", e.message, file=sys.stderr)
loop = GLib.MainLoop()
result = {}
token = "mynashot"
sender = bus.get_unique_name()[1:].replace(".", "_")
path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
def on_response(conn, s, p, i, sig, params):
    code, results = params.unpack()
    result["code"] = code; result["uri"] = results.get("uri")
    loop.quit()
bus.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", path, None, 0, on_response)
bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Screenshot",
    "Screenshot", GLib.Variant("(sa{sv})", ("", {"handle_token": GLib.Variant("s", token), "interactive": GLib.Variant("b", False)})),
    None, 0, 30000, None)
GLib.timeout_add_seconds(30, loop.quit)
loop.run()
if result.get("code") != 0 or not result.get("uri"):
    print("screenshot failed:", result, file=sys.stderr); sys.exit(1)
src = urllib.parse.unquote(urllib.parse.urlparse(result["uri"]).path)
shutil.move(src, out)
print(out)
