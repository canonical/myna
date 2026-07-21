// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator). Harness-tier; see
// specs/004-gnome-shell-indicator/contracts/extension.md.
//
// enable(): wire the org.myna.Dictation proxy (dbus.js) to the goop actor
// (indicator.js) added to Main.layoutManager — Shell chrome, never a window,
// so it can never steal keyboard focus (X11/SC-001). disable(): destroy all
// actors, cancel all timers/transitions, disconnect the proxy and the name
// watch (X9 — no leaks). Implementation lands with US1 (T016).
