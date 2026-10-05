// tests/watermarks.rs — the renderer's declared performance watermarks
// (feature 004, T152; plan.md Performance Goals / constitution III).
//
// These pin the DESIGN-CONTRACT constants that the plan's performance goals
// name, so a tuning regression (a slowed cadence, a duration drifting out
// of its declared band) fails loudly here rather than showing up as "the
// ribbon feels laggy" on hardware later. They are watermarks, not unit
// tests of correctness: they assert the constants live in their DECLARED
// ranges.
//
// Publisher watermarks are unchanged (T046 carried): this file covers only
// the renderer's own declared numbers.

use myna_hud::ribbon::{COMPLETE_MS, MORPH_MS, UNFOLD_MS};
use myna_hud::simulator::PUBLISH_HZ;
use myna_hud::states::{state_to_descriptor, wire};
use myna_hud::vumeter::{levels_to_intensity, STALE_MS};

// --- Declared timing constants stay in their documented ranges ----------

#[test]
fn activation_to_visible_is_immediate_no_extra_delay() {
    // The plan's activation-latency target: indicator visible within
    // ~100-200ms after State=recording is published. The renderer's pure
    // path adds ZERO latency of its own: a recording descriptor is visible
    // immediately, and the consumer forwards the state as soon as it
    // arrives. The only remaining time is the frame clock's next tick
    // (~16.7ms), well inside the target.
    let descriptor = state_to_descriptor(Some(wire::RECORDING), "Listening");
    assert!(
        !descriptor.hidden,
        "a recording descriptor is visible the moment it is applied"
    );
}

#[test]
fn stale_decay_window_is_the_declared_300ms() {
    assert_eq!(STALE_MS, 300.0, "stale-decay window is the declared 300ms");
}

#[test]
fn level_publish_cadence_is_15_to_20_hz() {
    // C4 / plan: AudioRms/AudioPeak updates throttled to ~15-20Hz.
    assert!(
        (15.0..=20.0).contains(&PUBLISH_HZ),
        "publish cadence within the declared 15-20Hz band: {PUBLISH_HZ}"
    );
}

// --- Lifecycle phase durations stay in their declared bands --------------

#[test]
fn lifecycle_durations_stay_in_band() {
    assert!(
        (150.0..=200.0).contains(&UNFOLD_MS),
        "unfold reveal within the 150-200ms band: {UNFOLD_MS}"
    );
    assert!(
        (200.0..=250.0).contains(&MORPH_MS),
        "morph within the 200-250ms band: {MORPH_MS}"
    );
    assert!(
        (300.0..=500.0).contains(&COMPLETE_MS),
        "complete within the 300-500ms band: {COMPLETE_MS}"
    );
}

#[test]
fn stale_quiet_decays_within_the_bounded_window() {
    // Plan: stale/quiet decay within the bounded window (~300ms stale).
    // A level that stops updating must fall to (near) the floor once the
    // arrival age passes STALE_MS, and not before.
    let fresh = levels_to_intensity(0.02, 0.04, 0.0);
    let just_before_stale = levels_to_intensity(0.02, 0.04, STALE_MS - 10.0);
    let after_stale = levels_to_intensity(0.02, 0.04, STALE_MS + 10.0);
    assert!(fresh > 0.5, "a fresh loud level is visibly up");
    assert!(
        just_before_stale > after_stale,
        "decay begins at the stale boundary, not before"
    );
    assert!(
        after_stale < 0.15,
        "a stale level has eased toward the floor: {after_stale}"
    );
}
