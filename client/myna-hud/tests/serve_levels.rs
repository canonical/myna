// tests/serve_levels.rs — the simulator's control→published-levels wiring
// (feature 004, T132). Pins the bug where the lab's audio slider updated the
// embedded preview but NOT the bus, so a shell-hosted instance's AudioRms
// sat frozen at the last state change. No bus needed: this exercises
// Shared::snapshot directly, exactly what the ~20 Hz publish loop reads.

// The `serve` module is dev-lab-only (#[cfg(dev_lab)]); skip this test when
// dev_lab is off (e.g. coverage builds, per build.rs / T171).
#![cfg(dev_lab)]

use myna_hud::serve::{Controls, Shared};
use myna_hud::simulator::envelope_to_levels;
use myna_hud::states::wire;

fn rms_for(shared: &Shared) -> f64 {
    let (_state, _status_message, rms, _peak) = shared.snapshot();
    rms
}

#[test]
fn a_changed_envelope_changes_the_published_levels() {
    let shared = Shared::default();

    // An active recording session at a low level.
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening".into(),
        envelope: 0.2,
    });
    let low = rms_for(&shared);

    // The slider moves up: the publish loop re-pushes the controls, and the
    // snapshot the bus reads must reflect the new envelope.
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening".into(),
        envelope: 0.8,
    });
    let high = rms_for(&shared);

    assert!(low > 0.0, "a live session publishes non-zero levels");
    assert!(
        high > low,
        "raising the envelope raises the published AudioRms: {low} -> {high}"
    );
}

#[test]
fn the_published_level_matches_the_calibration() {
    // The bus carries the envelope re-derived through envelope_to_levels,
    // the same calibration the embedded pill uses — so both agree for a
    // given slider value.
    let shared = Shared::default();
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening".into(),
        envelope: 0.55,
    });
    let (_s, _status_message, rms, peak) = shared.snapshot();
    let (expected_rms, expected_peak) = envelope_to_levels(0.55);
    assert!((rms - expected_rms).abs() < 1e-9, "{rms} vs {expected_rms}");
    assert!(
        (peak - expected_peak).abs() < 1e-9,
        "{peak} vs {expected_peak}"
    );
}

#[test]
fn a_live_status_message_override_is_published_verbatim() {
    let shared = Shared::default();
    shared.set_controls(Controls {
        state: wire::TRANSCRIBING.into(),
        status_message: "Decoding audio locally".into(),
        envelope: 0.4,
    });

    let (state, status_message, _rms, _peak) = shared.snapshot();
    assert_eq!(state, wire::TRANSCRIBING);
    assert_eq!(status_message, "Decoding audio locally");
}

#[test]
fn an_idle_session_publishes_no_levels() {
    let shared = Shared::default();
    shared.set_controls(Controls {
        state: wire::IDLE.into(),
        status_message: String::new(),
        envelope: 0.9, // even with the slider up
    });
    let (state, _status_message, rms, peak) = shared.snapshot();
    assert_eq!(state, wire::IDLE);
    assert_eq!(rms, 0.0, "no levels while idle");
    assert_eq!(peak, 0.0);
}

// --- The publish gate: off → on must resume publishing -----------------
// Pins the bug where toggling publish off then on in the lab never showed
// the pill again: start_publish early-returned when the name was already
// claimed, so the gate was never re-enabled. The gate and the claim are
// separate concerns — the claim happens once, the gate must re-open on
// every toggle-on.

#[test]
fn the_publish_gate_reenables_after_being_turned_off() {
    let shared = Shared::default();
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening".into(),
        envelope: 0.5,
    });

    // Publishing on by default.
    assert_eq!(shared.snapshot().0, wire::RECORDING);

    // Toggle off: the snapshot forces idle (consumers see the HUD go quiet).
    shared.set_publishing(false);
    let (state, _status_message, rms, _p) = shared.snapshot();
    assert_eq!(state, wire::IDLE, "gated off publishes idle");
    assert_eq!(rms, 0.0);

    // Toggle back on: publishing resumes — this is what was broken.
    shared.set_publishing(true);
    let (state, _status_message, rms, _p) = shared.snapshot();
    assert_eq!(
        state,
        wire::RECORDING,
        "re-enabled publishes the live state"
    );
    assert!(rms > 0.0, "levels flow again");
}
