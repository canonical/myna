//! Hermetic `DbusIndicator` suite (feature 004-gnome-shell-indicator) —
//! contract publisher.md P1–P5 / dbus-interface.md C2–C5 over the in-memory
//! [`FakeBus`]. No session bus required (research R11); the real bus round-trip
//! is the env-gated `dbus_hw.rs`.

use myna_desktop::dbus::{DictationService, FakeBus, PropertyValue};
use myna_desktop::indicator::dbus::{DbusIndicator, Readiness};
use myna_desktop::indicator::{Indicator, IndicatorState};

fn str_prop(value: &str) -> Option<PropertyValue> {
    Some(PropertyValue::Str(value.to_string()))
}

/// C2/P1: driving through a session emits exactly one `StateChanged` per
/// transition with the mapped wire state, and the `State` property tracks it.
#[tokio::test]
async fn one_signal_per_transition_and_state_property_tracks() {
    let fake = FakeBus::new();
    let service = DictationService::new(fake.clone());
    let readiness = Readiness::new();
    readiness.note_ready(); // warm session — Recording maps to `recording`
    let mut indicator = DbusIndicator::new(service.bus(), readiness);

    indicator.set_state(IndicatorState::Recording).await;
    assert_eq!(fake.property("State"), str_prop("recording"));

    indicator.set_state(IndicatorState::Transcribing).await;
    assert_eq!(fake.property("State"), str_prop("transcribing"));

    indicator.set_state(IndicatorState::Finalizing).await;
    assert_eq!(fake.property("State"), str_prop("finalizing"));

    indicator.hide().await;

    assert_eq!(
        fake.signals(),
        vec![
            ("recording".to_string(), String::new()),
            ("transcribing".to_string(), String::new()),
            ("finalizing".to_string(), String::new()),
            ("idle".to_string(), String::new()),
        ],
        "exactly one StateChanged per transition, in order"
    );
    assert_eq!(fake.property("State"), str_prop("idle"));
}

/// P3: `hide()` publishes `idle`, zeroes the levels, and clears `ErrorMessage`.
#[tokio::test]
async fn hide_publishes_idle_zeroes_levels_clears_error() {
    let fake = FakeBus::new();
    let service = DictationService::new(fake.clone());
    let readiness = Readiness::new();
    readiness.note_ready();
    let mut indicator = DbusIndicator::new(service.bus(), readiness);

    indicator
        .set_state(IndicatorState::Error("refusing to type into a password field".into()))
        .await;
    assert_eq!(
        fake.property("ErrorMessage"),
        str_prop("refusing to type into a password field")
    );

    indicator.hide().await;

    assert_eq!(fake.property("State"), str_prop("idle"));
    assert_eq!(fake.property("AudioRms"), Some(PropertyValue::F64(0.0)));
    assert_eq!(fake.property("AudioPeak"), Some(PropertyValue::F64(0.0)));
    assert_eq!(fake.property("ErrorMessage"), str_prop(""));
    assert_eq!(
        fake.signals().last(),
        Some(&("idle".to_string(), String::new()))
    );
}

/// P2/C5: a cold session publishes `loading` for the Loading-seen /
/// Ready-not-yet window, then `recording` once `Ready` arrives — even though
/// the controller maps both events to `IndicatorState::Recording`.
#[tokio::test]
async fn cold_load_publishes_loading_then_recording() {
    let fake = FakeBus::new();
    let service = DictationService::new(fake.clone());
    let readiness = Readiness::new();
    let mut indicator = DbusIndicator::new(service.bus(), readiness.clone());

    readiness.note_loading(); // OrchestratorEvent::Loading seen
    indicator.set_state(IndicatorState::Recording).await;
    assert_eq!(fake.property("State"), str_prop("loading"));

    readiness.note_ready(); // OrchestratorEvent::Ready seen
    indicator.set_state(IndicatorState::Recording).await;
    assert_eq!(fake.property("State"), str_prop("recording"));

    assert_eq!(
        fake.signals(),
        vec![
            ("loading".to_string(), String::new()),
            ("recording".to_string(), String::new()),
        ]
    );
}

/// C2: `set_state` is idempotent per wire state — re-delivering the same
/// mapped state (e.g. a duplicate `Recording`) emits no extra signal.
#[tokio::test]
async fn duplicate_states_do_not_reemit() {
    let fake = FakeBus::new();
    let service = DictationService::new(fake.clone());
    let readiness = Readiness::new();
    readiness.note_ready();
    let mut indicator = DbusIndicator::new(service.bus(), readiness);

    indicator.set_state(IndicatorState::Recording).await;
    indicator.set_state(IndicatorState::Recording).await;
    indicator.hide().await;
    indicator.hide().await;

    assert_eq!(
        fake.signals(),
        vec![
            ("recording".to_string(), String::new()),
            ("idle".to_string(), String::new()),
        ],
        "no StateChanged without a wire-state transition"
    );
}

/// C3: every emitted payload is a wire state + a content-free reason — the
/// signal args can only ever be the six state strings plus the reason string
/// from `IndicatorState::Error` (never transcript, which never reaches the
/// indicator seam).
#[tokio::test]
async fn payloads_are_content_free() {
    const WIRE_STATES: [&str; 6] = [
        "idle",
        "loading",
        "recording",
        "transcribing",
        "finalizing",
        "error",
    ];
    let fake = FakeBus::new();
    let service = DictationService::new(fake.clone());
    let mut indicator = DbusIndicator::new(service.bus(), Readiness::new());

    indicator.set_state(IndicatorState::Recording).await;
    indicator
        .set_state(IndicatorState::Error("inference backend unavailable".into()))
        .await;
    indicator.hide().await;

    for (state, _reason) in fake.signals() {
        assert!(WIRE_STATES.contains(&state.as_str()), "unknown wire state: {state}");
    }
}
