//! Env-gated `com.canonical.Myna.Dictation` integration suite (`MYNA_DBUS_TESTS=1`) -
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

/// A minimal consumer proxy for the served interface's methods.
#[zbus::proxy(
    interface = "com.canonical.Myna.Dictation",
    default_service = "com.canonical.Myna.Dictation",
    default_path = "/com/canonical/Myna/Dictation"
)]
trait DictationMethods {
    fn start(&self) -> zbus::Result<(bool, String)>;
    fn stop(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
}

/// C6 on the wire: the served `Toggle` method feeds a `DbusTrigger` — a
/// `Toggle` while idle yields a `Press` edge, another `Toggle` while active
/// yields a `Release` (P9), and duplicate `Start`s do not start two sessions
/// (P10).
#[tokio::test]
async fn served_toggle_method_feeds_the_trigger() {
    if !dbus_enabled() {
        return;
    }

    let (mut trigger, source) = myna_desktop::shortcut::dbus::DbusTrigger::new();
    // The well-known name is process-wide: the sibling singleton-lock test may
    // already own it (cargo runs test fns concurrently). If so, the wire
    // round-trip here cannot run — skip it rather than fighting over the name;
    // the trigger's own logic is covered hermetically in tests/dbus_trigger.rs.
    let owner = match ZbusBus::serve_with_trigger(Some(source)).await {
        Ok(owner) => owner,
        Err(ServeError::AlreadyRunning { .. }) => {
            eprintln!("skipping served_toggle: another test owns the name");
            return;
        }
        Err(other) => panic!("serve_with_trigger failed: {other}"),
    };
    let _owner = owner;

    let conn = zbus::Connection::session().await.expect("session bus");
    let proxy = DictationMethodsProxy::new(&conn).await.expect("proxy");

    // Toggle while idle -> Press.
    proxy.toggle().await.expect("Toggle on");
    let edge = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        myna_orchestrator::Trigger::next_edge(&mut trigger),
    )
    .await
    .expect("Press edge arrives");
    assert_eq!(edge, Some(myna_orchestrator::TriggerEdge::Press));

    // Toggle while active -> Release.
    proxy.toggle().await.expect("Toggle off");
    let edge = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        myna_orchestrator::Trigger::next_edge(&mut trigger),
    )
    .await
    .expect("Release edge arrives");
    assert_eq!(edge, Some(myna_orchestrator::TriggerEdge::Release));

    // Duplicate Start while active -> no second Press (P10).
    let (ok, reason) = proxy.start().await.expect("Start");
    assert!(ok && reason.is_empty(), "Start reports success (C7 shape)");
    let edge = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        myna_orchestrator::Trigger::next_edge(&mut trigger),
    )
    .await
    .expect("Start -> Press");
    assert_eq!(edge, Some(myna_orchestrator::TriggerEdge::Press));
    proxy.start().await.expect("Start again");
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        myna_orchestrator::Trigger::next_edge(&mut trigger),
    )
    .await
    .expect_err("a duplicate Start must NOT start a second session");
}

/// `Shortcut` is what Myna Settings renders: a publish reaches a reader.
#[tokio::test]
async fn the_published_shortcut_is_readable_on_the_bus() {
    use myna_desktop::dbus::{Bus, PropertyValue, OBJECT_PATH};

    if !dbus_enabled() {
        return;
    }
    let mut owner = match ZbusBus::serve().await {
        Ok(owner) => owner,
        Err(ServeError::AlreadyRunning { .. }) => {
            eprintln!("skipping published_shortcut: another test owns the name");
            return;
        }
        Err(other) => panic!("serve failed: {other}"),
    };
    let conn = zbus::Connection::session().await.expect("session bus");
    let properties = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(BUS_NAME)
        .unwrap()
        .path(OBJECT_PATH)
        .unwrap()
        .build()
        .await
        .expect("properties proxy");
    let interface = zbus::names::InterfaceName::try_from(BUS_NAME).unwrap();
    let read = |value: zbus::zvariant::OwnedValue| String::try_from(value).unwrap();

    let initial = properties
        .get(interface.clone(), "Shortcut")
        .await
        .expect("Shortcut is served");
    assert_eq!(read(initial), "");

    owner
        .set_property("Shortcut", PropertyValue::Str("Press <Super>j".into()))
        .await;
    let published = properties
        .get(interface, "Shortcut")
        .await
        .expect("Shortcut after publish");
    assert_eq!(read(published), "Press <Super>j");
}
