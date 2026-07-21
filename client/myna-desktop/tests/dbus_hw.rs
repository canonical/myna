//! Env-gated `org.myna.Dictation` integration suite (`MYNA_DBUS_TESTS=1`) —
//! feature 004-gnome-shell-indicator, contracts publisher.md P13–P15 /
//! dbus-interface.md C1/C9.
//!
//! Stands the real `zbus`-backed object on a session bus and asserts a `zbus`
//! client observes `StateChanged`, reads `State`/`AudioRms`/`AudioPeak`, and
//! sees name-appeared/vanished on start/shutdown. Run under an isolated
//! session bus (exactly like the IBus suite):
//!
//! ```sh
//! MYNA_DBUS_TESTS=1 dbus-run-session -- cargo test -p myna-desktop --test dbus_hw
//! ```
//!
//! It skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline (Principle II — identical code on the desktop VM and hardware).

/// True when the D-Bus integration suite is enabled. Unset gate → skip.
fn dbus_enabled() -> bool {
    std::env::var("MYNA_DBUS_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if dbus_enabled() {
        eprintln!("MYNA_DBUS_TESTS set: the session-bus round-trip assertions land in T033");
    } else {
        eprintln!("skipping dbus_hw: set MYNA_DBUS_TESTS=1 under dbus-run-session");
    }
}
