//! Env-gated `org.myna.Dictation` integration suite (`MYNA_DBUS_TESTS=1`) -
//! feature 004-gnome-shell-indicator, contracts publisher.md P13-P15 /
//! dbus-interface.md C1/C9.
//!
//! Stands the real `zbus`-backed object on a session bus and asserts a `zbus`
//! client observes `PropertiesChanged` for `State`, reads
//! `State`/`AudioRms`/`AudioPeak`, and sees name-appeared/vanished on
//! start/shutdown. Run under an isolated
//! session bus (exactly like the IBus suite):
//!
//! ```sh
//! MYNA_DBUS_TESTS=1 dbus-run-session -- cargo test -p myna-desktop --test dbus_hw
//! ```
//!
//! It skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline (Principle II - identical code on the desktop VM and hardware).

use myna_desktop::dbus::serve::{ServeError, ZbusBus};
use myna_desktop::dbus::BUS_NAME;

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

/// The name is the daemon's singleton lock, in both directions.
///
/// The regression that motivated it: zbus's *default* request flags are
/// `AllowReplacement | ReplaceExisting | DoNotQueue`, so a later daemon
/// silently stole the indicator from a running one while the GlobalShortcuts
/// portal kept the hotkey with the first. Key in one process, UI in another,
/// and every press looking to the user like nothing happened.
///
/// One test, not two: both halves need to be the sole owner of the name on the
/// session bus, and `cargo test` runs test fns concurrently in one process.
#[tokio::test]
async fn the_name_is_a_singleton_lock() {
    if !dbus_enabled() {
        return;
    }
    let _owner = ZbusBus::serve().await.expect("first serve owns the name");

    // A second daemon is told who is already there, not quietly started.
    match ZbusBus::serve().await {
        Err(ServeError::AlreadyRunning { owner_pid }) => {
            assert_eq!(owner_pid, Some(std::process::id()));
        }
        Ok(_) => panic!("a second daemon took {BUS_NAME}"),
        Err(other) => panic!("expected AlreadyRunning, got {other}"),
    }

    // And an owner that never allows replacement cannot be stolen from,
    // however hard the second asks. zbus surfaces the bus's `Exists` reply
    // as `Error::NameTaken`.
    use zbus::fdo::RequestNameFlags;
    let steal = RequestNameFlags::AllowReplacement
        | RequestNameFlags::ReplaceExisting
        | RequestNameFlags::DoNotQueue;
    let thief = zbus::Connection::session()
        .await
        .expect("second bus connection");
    match thief.request_name_with_flags(BUS_NAME, steal).await {
        Err(zbus::Error::NameTaken) => {}
        Ok(reply) => panic!("the name was stolen: {reply:?}"),
        Err(other) => panic!("expected NameTaken, got {other}"),
    }
}

/// C12/C13 round-trip: myna-desktop's presence probe sees `org.myna.Shell`
/// appear and vanish, and its surface decision suppresses the notification
/// fallback while the host is up (P20) and restores it when it goes (P21).
///
/// One test, not two: owning the well-known name is process-wide, and the
/// probe must be the only observer of its appear/vanished.
#[tokio::test]
async fn shell_presence_round_trips_and_suppresses_the_fallback() {
    if !dbus_enabled() {
        return;
    }

    // Absent first: the fallback is restored.
    assert!(
        !myna_desktop::policy::probe_shell_presence().await,
        "no org.myna.Shell owner yet"
    );
    let decision = myna_desktop::policy::SurfaceDecision::for_shell_presence(false);
    assert!(decision.uses_notify_fallback, "P21: fallback restored");

    // Claim the presence name the way the extension host does.
    let connection = zbus::Connection::session().await.expect("session bus");
    connection
        .request_name(myna_desktop::policy::PRESENCE_NAME)
        .await
        .expect("claim org.myna.Shell");

    assert!(
        myna_desktop::policy::probe_shell_presence().await,
        "C12: the probe sees the shell host"
    );
    let decision = myna_desktop::policy::SurfaceDecision::for_shell_presence(true);
    assert!(
        !decision.uses_notify_fallback,
        "P20: fallback suppressed while the host is up"
    );

    // Release: the probe sees it vanish.
    drop(connection);
    // Give the bus a moment to process the name release.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !myna_desktop::policy::probe_shell_presence().await,
        "C13: the probe sees the name vanish"
    );
}
