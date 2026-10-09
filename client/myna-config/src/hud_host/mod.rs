//! The HUD host for sessions with no shell extension (Xfce): a supervisor
//! that runs `myna-hud --host x11` only while the dictation daemon owns its
//! bus name, so an idle HUD never blocks a snap refresh (issue #38). It is the
//! GNOME extension's role (`extensions/myna-shell/host.js`) for X11, started
//! by an XDG autostart entry.

pub mod launch;
pub mod machine;
pub mod run;

/// The daemon's well-known bus name (`myna-desktop/src/dbus`).
pub const DAEMON_BUS_NAME: &str = "com.canonical.Myna.Dictation";
