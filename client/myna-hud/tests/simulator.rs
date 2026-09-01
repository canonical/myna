// tests/simulator.rs — hermetic test for the --serve-dbus simulator's pure
// mapping (lab controls → com.canonical.Myna.Dictation wire properties).
// The drift checks round-trip through the REAL vumeter math to catch
// calibration drift between the slider and the rendered ribbon.

use myna_hud::ribbon::RibbonPhase;
use myna_hud::simulator::{envelope_to_levels, shell_phase, wire_state, ERROR_MESSAGE, PUBLISH_HZ};
use myna_hud::states::{wire, Severity};
use myna_hud::vumeter::levels_to_intensity;

// --- wire_state: phase/severity/session → (State, StatusMessage) ----------

#[test]
fn inactive_session_is_idle() {
    // Stop()/Toggle() ended the session — the daemon is still running, it
    // is simply not dictating, which is the case that clears the pill.
    assert_eq!(
        wire_state("flow", None, false),
        (wire::IDLE, ""),
        "session_active=false → idle, no reason"
    );
    assert_eq!(
        wire_state("flow", Some(Severity::Critical), false),
        (wire::IDLE, ""),
        "session end outranks severity"
    );
}

#[test]
fn severity_outranks_phase() {
    // The pill drives notice/error from the state itself, not from a ribbon
    // phase, so a tinted ribbon has to publish the matching state or the
    // pill would stay neutral while only the lab's ribbon went amber.
    assert_eq!(
        wire_state("flow", Some(Severity::Recoverable), true),
        (wire::NOTICE, "No speech detected"),
        "recoverable → notice with the publisher-owned status message"
    );
    assert_eq!(
        wire_state("morph", Some(Severity::Critical), true),
        (wire::ERROR, ERROR_MESSAGE),
        "critical → error with a content-free reason"
    );
}

#[test]
fn phases_map_to_their_states() {
    // The inverse of hud_logic's phase-for-state mapping — many-to-one
    // (unfold/flow both sit inside a recording session; `recording` is
    // chosen because it is the state a person watching the ribbon flow is
    // actually in).
    assert_eq!(wire_state("unfold", None, true).0, wire::RECORDING);
    assert_eq!(wire_state("flow", None, true).0, wire::RECORDING);
    assert_eq!(wire_state("morph", None, true).0, wire::TRANSCRIBING);
    assert_eq!(wire_state("complete", None, true).0, wire::FINALIZING);
}

#[test]
fn unknown_phase_degrades_to_active() {
    // The same additive tolerance the contract asks of clients (C8) — a
    // phase added to the ribbon later shows up as a neutral live state
    // rather than breaking the publisher.
    assert_eq!(wire_state("relax", None, true).0, "active");
    assert_eq!(wire_state("quantize", None, true).0, "active");
}

// --- shell_phase: the round-trip explanation mapping ----------------------

#[test]
fn shell_phase_round_trips() {
    // Every phase either round-trips through the wire and the consumer's
    // own state → phase mapping, or is Shell/renderer-internal (unfold: the
    // renderer plays the reveal itself on a fresh session, on its own
    // clock).
    assert_eq!(shell_phase("flow", None, true), Some(RibbonPhase::Flow));
    assert_eq!(shell_phase("morph", None, true), Some(RibbonPhase::Morph));
    assert_eq!(
        shell_phase("complete", None, true),
        Some(RibbonPhase::Complete)
    );
    assert_eq!(
        shell_phase("unfold", None, true),
        Some(RibbonPhase::Flow),
        "unfold publishes recording; the renderer plays the reveal itself"
    );
    // Severity states force no phase — the severity carries them instead.
    assert_eq!(shell_phase("flow", Some(Severity::Recoverable), true), None);
    assert_eq!(shell_phase("flow", Some(Severity::Critical), true), None);
    // An ended session leaves nothing to show.
    assert_eq!(shell_phase("flow", None, false), None);
}

// --- envelope_to_levels: invert the vumeter so the slider is 1:1 ----------

#[test]
fn envelope_round_trips_through_the_real_vumeter() {
    // The slider is the smoothed envelope; the wire carries raw RMS/peak
    // which the consumer pushes back through levels_to_intensity. This
    // deliberate transcription of the calibration is what makes the lab's
    // ribbon and the hosted ribbon agree — and what catches drift if the
    // vumeter constants ever change without the simulator following.
    for level in [0.05, 0.2, 0.5, 0.8, 1.0] {
        let (rms, peak) = envelope_to_levels(level);
        let intensity = levels_to_intensity(rms, peak, 0.0);
        assert!(
            (intensity - level).abs() < 1e-9,
            "slider {level} → rms {rms:.4} → intensity {intensity:.4}"
        );
    }
}

#[test]
fn envelope_zero_is_zero_and_values_are_legal() {
    assert_eq!(envelope_to_levels(0.0), (0.0, 0.0));
    for level in [-0.5, 0.5, 1.7] {
        let (rms, peak) = envelope_to_levels(level);
        assert!(
            (0.0..=1.0).contains(&rms) && (0.0..=1.0).contains(&peak),
            "clamped inputs stay in range"
        );
    }
}

#[test]
fn publish_rate_matches_the_contract_cadence() {
    // ~15-20 Hz per C4 — 20 keeps the consumer seeing the update rate it
    // was tuned against rather than the lab's render-loop rate.
    assert!((15.0..=20.0).contains(&PUBLISH_HZ));
}
