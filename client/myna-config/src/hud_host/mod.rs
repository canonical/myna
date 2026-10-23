//! The HUD host for sessions with no shell extension (Xfce, Plasma): a
//! supervisor that runs `myna-hud --host x11` or `--host layer-shell` only
//! while the dictation daemon owns its bus name, so an idle HUD never blocks a
//! snap refresh (issue #38). It is the GNOME extension's role
//! (`extensions/myna-shell/host.js`) elsewhere, started by an XDG autostart
//! entry.

pub mod launch;
pub mod machine;
pub mod run;

/// The daemon's well-known bus name (`myna-desktop/src/dbus`).
pub const DAEMON_BUS_NAME: &str = "com.canonical.Myna.Dictation";
/// Where it serves its interface of the same name.
pub const DAEMON_OBJECT_PATH: &str = "/com/canonical/Myna/Dictation";
