// dbus.js — Gio.DBusProxy for org.myna.Dictation with Gio.bus_watch_name
// lifecycle (feature 004, contracts dbus-interface.md / extension.md X7–X10).
//
// Dormant while the name has no owner; activates on name-appeared (reflecting
// the current State) and clears to idle on name-vanished (daemon crash/exit).
// Exposes state/errorMessage + a StateChanged callback to the actor layer.
// Implementation lands with US1 (T015).
export {};
