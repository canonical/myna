// tests/hud_logic.rs — hermetic contract test for the HUD pill's PURE logic
// (feature 004, 2026-07-30 HUD redesign; contracts extension.md RC19–RC21).
// Exercises hud_logic only — no GTK. The app's composited behaviour
// (focus-safety, timing, per-state treatments, dismiss click) is
// manual-acceptance plus the env-gated render check.

use myna_hud::hud_logic::{
    icon_for_severity, indicator_state, pill_color_class, pulse_position,
    ribbon_phase_for_state_key, ribbon_visible_for_severity, severity_auto_dismisses,
    should_replace_held_notice, smooth_level, PILL_COLOR_CLASSES,
};
use myna_hud::ribbon::RibbonPhase;
use myna_hud::states::{DictationState, Severity};

// --- RC19: mic vs. mic-slash icon, contextual on severity -------------------

#[test]
fn x19_icon_by_severity() {
    assert_eq!(
        icon_for_severity(Some(Severity::Critical)),
        "microphone-disabled-symbolic",
        "critical → mic-slash"
    );
    assert_eq!(
        icon_for_severity(Some(Severity::Recoverable)),
        "audio-input-microphone-symbolic",
        "recoverable → plain mic (the mic itself is not at fault)"
    );
    assert_eq!(
        icon_for_severity(None),
        "audio-input-microphone-symbolic",
        "no severity (loading/recording/...) → plain mic"
    );
}

// --- FR-007a/FR-007b: auto-dismiss behavior by severity --------------------
// Only Recoverable (notice) auto-dismisses locally; Critical (error)
// stays until server publishes new state. Server auto-dismisses notice
// after longer hold, client keeps showing for even longer (slower reading)
// and ignores server idle until its own timer completes.

#[test]
fn auto_dismiss_by_severity() {
    assert!(severity_auto_dismisses(Some(Severity::Recoverable)));
    assert!(!severity_auto_dismisses(Some(Severity::Critical)));
    assert!(
        !severity_auto_dismisses(None),
        "non-problem states have no auto-dismiss concept"
    );
}

// --- RC20: replace-in-place — any new problem descriptor replaces the held
// --- slot; there is never a queue, regardless of matching severity. --------

#[test]
fn x20_replace_in_place() {
    assert!(should_replace_held_notice(Some(Severity::Recoverable)));
    assert!(should_replace_held_notice(Some(Severity::Critical)));
    assert!(
        !should_replace_held_notice(None),
        "a non-problem state does not \"replace\" (nothing to hold)"
    );
}

// --- Manual-test follow-up: severity/phase colour classes ------------------

#[test]
fn color_classes() {
    assert_eq!(
        pill_color_class(DictationState::Notice, Some(Severity::Recoverable)),
        Some("myna-hud-severity-recoverable"),
        "recoverable → orange colour class"
    );
    assert_eq!(
        pill_color_class(DictationState::Error, Some(Severity::Critical)),
        Some("myna-hud-severity-critical"),
        "critical → red colour class"
    );
    assert_eq!(
        pill_color_class(DictationState::Loading, None),
        Some("myna-hud-phase-loading"),
        "loading → warm phase colour class"
    );
    for key in [
        DictationState::Recording,
        DictationState::Transcribing,
        DictationState::Finalizing,
    ] {
        assert_eq!(
            pill_color_class(key, None),
            None,
            "{key:?} → no colour override"
        );
    }
    assert!(
        PILL_COLOR_CLASSES.contains(&"myna-hud-severity-recoverable")
            && PILL_COLOR_CLASSES.contains(&"myna-hud-severity-critical")
            && PILL_COLOR_CLASSES.contains(&"myna-hud-phase-loading"),
        "every possible colour class is listed in PILL_COLOR_CLASSES"
    );
}

// --- R17 / 2026-08-21 fix: which state keys force a phase ------------------

#[test]
fn phase_for_state_key() {
    assert_eq!(
        ribbon_phase_for_state_key(DictationState::Transcribing),
        Some(RibbonPhase::Morph),
        "transcribing forces the ribbon into morph"
    );
    assert_eq!(
        ribbon_phase_for_state_key(DictationState::Finalizing),
        Some(RibbonPhase::Complete),
        "finalizing forces the ribbon into complete (FR-010d)"
    );
    // Live states pin the ribbon to flow — this is what recovers it after a
    // morph/complete, which was previously stuck until idle/a new session.
    for key in [
        DictationState::Loading,
        DictationState::Recording,
        DictationState::Active,
    ] {
        assert_eq!(
            ribbon_phase_for_state_key(key),
            Some(RibbonPhase::Flow),
            "{key:?} forces the ribbon into flow"
        );
    }
    // idle never shows; notice/error are carried by tint/visibility, not phase.
    for key in [
        DictationState::Idle,
        DictationState::Notice,
        DictationState::Error,
    ] {
        assert_eq!(
            ribbon_phase_for_state_key(key),
            None,
            "{key:?} does not force a phase"
        );
    }
}

// --- R17a: ribbon visibility by severity (only critical hides) -------------

#[test]
fn ribbon_visibility_by_severity() {
    assert!(
        ribbon_visible_for_severity(Some(Severity::Recoverable)),
        "stays visible for a recoverable notice (amber/paused instead of hidden)"
    );
    assert!(
        !ribbon_visible_for_severity(Some(Severity::Critical)),
        "hides for a critical error"
    );
    assert!(
        ribbon_visible_for_severity(None),
        "stays visible for non-problem states"
    );
}

// --- Simple-indicator state animation (bar / vumeter / progress) -----------

#[test]
fn plain_level_states_report_the_raw_level() {
    let s = indicator_state(DictationState::Recording, None, 0.42, 0.0, false);
    assert_eq!(s.fraction, 0.42);
    assert!(s.pulse.is_none());
    assert!(!s.warning);
}

#[test]
fn loading_transcribing_finalizing_report_a_pulse() {
    for (key, period) in [
        (DictationState::Loading, 2000.0),
        (DictationState::Transcribing, 1400.0),
        (DictationState::Finalizing, 1000.0),
    ] {
        let s = indicator_state(key, None, 0.0, 0.0, false);
        let Some(pulse) = s.pulse else {
            panic!("{key:?} must pulse");
        };
        assert!(pulse.width > 0.0 && pulse.width <= 1.0);
        assert_eq!(pulse.period_ms, period);
        assert!(!s.warning);
    }
}

#[test]
fn reduced_motion_pulses_are_slower_but_still_move() {
    let normal = |key| {
        let s = indicator_state(key, None, 0.0, 0.0, false);
        s.pulse.unwrap().period_ms
    };
    let reduced = |key| {
        let s = indicator_state(key, None, 0.0, 0.0, true);
        s.pulse.unwrap().period_ms
    };
    for key in [
        DictationState::Loading,
        DictationState::Transcribing,
        DictationState::Finalizing,
    ] {
        assert!(
            reduced(key) > normal(key) * 3.0,
            "{key:?} still pulses under reduced motion, just slower"
        );
    }
}

#[test]
fn loading_pulse_is_semi_transparent() {
    let loading = indicator_state(DictationState::Loading, None, 0.0, 0.0, false);
    let transcribing = indicator_state(DictationState::Transcribing, None, 0.0, 0.0, false);
    assert!(
        loading.pulse.unwrap().alpha < 1.0,
        "loading uses a semi-transparent accent"
    );
    assert!(
        transcribing.pulse.unwrap().alpha == 1.0,
        "transcribing is a full solid accent"
    );
}

#[test]
fn notice_reports_warning_empty() {
    let s = indicator_state(
        DictationState::Notice,
        Some(Severity::Recoverable),
        0.0,
        0.0,
        false,
    );
    assert!(s.warning);
    assert_eq!(s.fraction, 0.0, "a notice bar is empty, not full");
}

#[test]
fn critical_is_closed_and_not_warning() {
    let s = indicator_state(
        DictationState::Error,
        Some(Severity::Critical),
        0.5,
        0.0,
        false,
    );
    assert_eq!(s.fraction, 0.0);
    assert!(s.pulse.is_none());
    assert!(!s.warning);
}

#[test]
fn pulse_position_swings_back_and_forth() {
    // 0 at t=0, 1 at t=period/2, 0 again at t=period (the pong).
    assert_eq!(pulse_position(0.0, 1000.0), 0.0);
    assert_eq!(pulse_position(500.0, 1000.0), 1.0);
    assert!((pulse_position(1000.0, 1000.0) - 0.0).abs() < 1e-9);
    // Symmetric around the peak.
    assert!((pulse_position(250.0, 1000.0) - pulse_position(750.0, 1000.0)).abs() < 1e-9);
}

// --- smooth_level: sample-intensity easing ----------------------------------

#[test]
fn smooth_level_is_identity_with_no_elapsed_time() {
    assert_eq!(
        smooth_level(0.0, 0.5, 0.0, false),
        0.5,
        "dt=0 jumps to target"
    );
}

#[test]
fn smooth_level_eases_toward_the_target() {
    // Attack (rising): a small step moves partway toward the target, and a
    // much bigger dt converges to it.
    let a = smooth_level(0.0, 1.0, 50.0, false);
    assert!(a > 0.0 && a < 1.0, "rises partway: {a}");
    assert!(
        smooth_level(0.0, 1.0, 2000.0, false) > a,
        "more time converges closer to the target"
    );
}

#[test]
fn smooth_level_is_slower_under_reduced_motion() {
    // Same dt: reduced motion eases less far than full motion.
    let full = smooth_level(0.0, 1.0, 200.0, false);
    let reduced = smooth_level(0.0, 1.0, 200.0, true);
    assert!(
        reduced < full,
        "reduced motion eases the level more slowly (got reduced={reduced}, full={full})"
    );
}

#[test]
fn smooth_level_clamps_and_snaps_on_down_motion() {
    // A fresh start (previous 0) to a 0.2 sample with no dt snaps.
    assert_eq!(smooth_level(0.0, 0.2, 0.0, false), 0.2);
    // Rising toward an existing target never overshoots 1.
    assert!(smooth_level(0.0, 1.0, 1e9, false) <= 1.0);
}
