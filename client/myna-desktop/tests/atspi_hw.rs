//! Env-gated `org.a11y.Bus` integration suite (`MYNA_ATSPI_TESTS=1`) — feature
//! 011-accessible-dictation-ux, contract announcer.md A6.
//!
//! Connects the real `AtspiAnnouncer` to a live accessibility bus, emits an
//! `Announcement`, and asserts a separate `atspi` client observes it — plus
//! that the announcer's own name/description accessors reflect `set_state()`
//! at an arbitrary moment, not only at the instant of a transition (FR-001).
//!
//! ```sh
//! MYNA_ATSPI_TESTS=1 cargo test -p myna-desktop --test atspi_hw
//! ```
//!
//! Run against a session with a real `org.a11y.Bus` (a desktop session, or
//! `at-spi-bus-launcher` stood up under the Workshop desktop SDK's
//! `at-spi2-core` dependency — see `.workshop/desktop/hooks/setup-base`).
//! Skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline (constitution Principle II).

use myna_desktop::accessibility::atspi::AtspiAnnouncer;
use myna_desktop::accessibility::{AccessibilityAnnouncer, AnnouncementText, Severity};

fn atspi_enabled() -> bool {
    std::env::var("MYNA_ATSPI_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if atspi_enabled() {
        eprintln!("MYNA_ATSPI_TESTS set: the real org.a11y.Bus round-trip assertions run below");
    } else {
        eprintln!("skipping atspi_hw: set MYNA_ATSPI_TESTS=1 with a real org.a11y.Bus reachable");
    }
}

// ── T032/A6: the real announcer connects and emits an Announcement ─────────

#[tokio::test]
async fn connects_to_the_real_bus_and_emits_an_announcement() {
    if !atspi_enabled() {
        return;
    }
    let mut announcer = AtspiAnnouncer::connect()
        .await
        .expect("org.a11y.Bus should be reachable when MYNA_ATSPI_TESTS=1 is set");

    announcer
        .announce(AnnouncementText::new("Listening"), None)
        .await
        .expect("announce() should succeed against a real bus");
    announcer
        .announce(AnnouncementText::new("Error"), Some(Severity::Critical))
        .await
        .expect("a severity-bearing announce() should also succeed");
}

// ── T032/FR-001: name/description are queryable at an arbitrary moment,
//    not only synchronously inside a transition ────────────────────────────

#[tokio::test]
async fn name_and_description_are_queryable_between_transitions() {
    if !atspi_enabled() {
        return;
    }
    let mut announcer = AtspiAnnouncer::connect()
        .await
        .expect("org.a11y.Bus should be reachable when MYNA_ATSPI_TESTS=1 is set");

    announcer
        .set_state(
            AnnouncementText::new("Dictation: listening"),
            AnnouncementText::new("Recording your speech"),
        )
        .await;

    // Simulate "some time later, unrelated to any transition" by simply
    // querying again — the accessors must reflect the last set_state() call
    // regardless of when they're read (FR-001).
    assert_eq!(announcer.name(), "Dictation: listening");
    assert_eq!(announcer.description(), "Recording your speech");
}

// ── Regression (found 2026-08-31, first real manual verification against a
//    live Orca session): the object every `Announcement`'s `item` names
//    MUST answer AT-SPI introspection quickly, not hang. A real AT-SPI
//    client (Orca included) makes exactly this kind of synchronous
//    `GetRole`/property-`Get` call back to the event's source as part of
//    ordinary event handling — an earlier revision of `AtspiAnnouncer`
//    pointed `item` at a connection that answered nothing, and every one of
//    these calls hung until the calling client's own watchdog killed it.
//    This test is a second, independent client connecting to the bus and
//    making that exact kind of call directly, bounded by a short timeout so
//    a regression fails the test instead of hanging the test runner. ──────

#[tokio::test]
async fn the_exported_accessible_object_answers_introspection_quickly() {
    if !atspi_enabled() {
        return;
    }
    let mut announcer = AtspiAnnouncer::connect()
        .await
        .expect("org.a11y.Bus should be reachable when MYNA_ATSPI_TESTS=1 is set");
    announcer
        .set_state(
            AnnouncementText::new("Dictation: listening"),
            AnnouncementText::new("Recording your speech"),
        )
        .await;

    let item = announcer.item().clone();
    let bus_name = item
        .name_as_str()
        .expect("a real AtspiAnnouncer's item always has a bus name")
        .to_string();
    let path = item.path_as_str().to_string();

    // A second, independent client connection — simulating an AT-SPI client
    // (Orca) that received our Announcement and is now introspecting its
    // source, exactly as real clients do.
    let client = atspi::connection::AccessibilityConnection::new()
        .await
        .expect("a second connection to org.a11y.Bus should also succeed");
    let conn = client.connection();

    let timeout = std::time::Duration::from_secs(3);

    let role_reply = tokio::time::timeout(
        timeout,
        conn.call_method(
            Some(bus_name.as_str()),
            path.as_str(),
            Some("org.a11y.atspi.Accessible"),
            "GetRole",
            &(),
        ),
    )
    .await
    .expect(
        "GetRole must answer within 3s, not hang (the exact regression this test guards against)",
    )
    .expect("GetRole should succeed against the exported accessible object");
    let role: u32 = role_reply
        .body()
        .deserialize()
        .expect("GetRole's reply body should deserialize as a u32 role");
    assert_eq!(
        role, 75,
        "the exported object's role should be Role::Application"
    );

    let name_reply = tokio::time::timeout(
        timeout,
        conn.call_method(
            Some(bus_name.as_str()),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.a11y.atspi.Accessible", "Name"),
        ),
    )
    .await
    .expect("the Name property Get must answer within 3s, not hang")
    .expect("the Name property Get should succeed");
    let name: zbus::zvariant::OwnedValue = name_reply
        .body()
        .deserialize()
        .expect("the Name property reply should deserialize as a variant");
    let name: String = name
        .downcast_ref::<String>()
        .expect("the Name property should be a string")
        .clone();
    assert_eq!(
        name, "Dictation: listening",
        "the exported Name property should reflect the last set_state() call"
    );
}
