// tests/watermarks.rs — the renderer's declared performance watermarks
// (feature 004, T152; plan.md Performance Goals / constitution III).
//
// These pin the DESIGN-CONTRACT constants that the plan's performance goals
// name, so a tuning regression (a retuned tau, a slowed cadence, a duration
// drifting out of its declared band) fails loudly here rather than showing
// up as "the ribbon feels laggy" on hardware later. They are watermarks,
// not unit tests of correctness: they assert the constants live in their
// DECLARED ranges and that the derived frame-clock behaviour stays within
// tolerance.
//
// Publisher watermarks are unchanged (T046 carried): this file covers only
// the renderer's own declared numbers.

use myna_hud::ribbon::{
    apply_envelope_smoothing, ATTACK_TAU_MS, COMPLETE_MS, MORPH_MS, RELEASE_TAU_MS,
    SMOOTHING_TAU_MS, UNFOLD_MS,
};
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
    let descriptor = state_to_descriptor(Some(wire::RECORDING), "");
    assert!(
        !descriptor.hidden,
        "a recording descriptor is visible the moment it is applied"
    );
}

#[test]
fn envelope_constants_match_the_declared_ballistics() {
    // R17f: attack 35ms / release 280ms — the "more reactive" fast-rise,
    // slow-decay pair. These are the declared watermark values.
    assert_eq!(ATTACK_TAU_MS, 35.0, "attack tau is the declared 35ms");
    assert_eq!(
        SMOOTHING_TAU_MS, 280.0,
        "release/smoothing tau is the declared 280ms"
    );
    assert_eq!(RELEASE_TAU_MS, SMOOTHING_TAU_MS);
    const _: () = assert!(ATTACK_TAU_MS < RELEASE_TAU_MS);
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

// --- Frame-budget: the 60fps clock stays responsive ----------------------

#[test]
fn a_frame_advances_the_envelope_a_bounded_amount() {
    // At 60fps a frame is ~16.7ms. The envelope must move a meaningful but
    // bounded amount per frame on the attack side — enough to feel
    // responsive, not so much it's a step function (that would read as
    // jitter). This is the "~60fps without blocking" watermark made
    // concrete: a single frame must not jump the full range.
    let per_frame_attack = apply_envelope_smoothing(0.0, 1.0, 16.7);
    assert!(
        (0.2..0.6).contains(&per_frame_attack),
        "one 60fps frame advances attack a bounded amount: {per_frame_attack}"
    );

    // At that rate the envelope converges to (near) full loudness within
    // the activation target (~100-200ms → ~6-12 frames).
    let mut envelope = 0.0;
    for _ in 0..12 {
        envelope = apply_envelope_smoothing(envelope, 1.0, 16.7);
    }
    assert!(
        envelope > 0.9,
        "after 12 frames (~200ms) the envelope is effectively full: {envelope}"
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
