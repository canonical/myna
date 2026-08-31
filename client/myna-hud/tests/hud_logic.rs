// tests/hud_logic.rs — hermetic contract test for the HUD pill's PURE logic
// (feature 004, 2026-07-30 HUD redesign; contracts extension.md X19–X21).
// Exercises hud_logic only — no GTK. The app's composited behaviour
// (focus-safety, timing, per-state treatments, dismiss click) is
// manual-acceptance plus the env-gated render check.

use myna_hud::hud_logic::{
    icon_for_severity, pill_color_class, ribbon_phase_for_state_key, ribbon_visible_for_severity,
    severity_auto_dismisses, should_replace_held_notice, PILL_COLOR_CLASSES,
};
use myna_hud::ribbon::RibbonPhase;
use myna_hud::states::{DictationState, Severity};

// --- X19: mic vs. mic-slash icon, contextual on severity -------------------

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

// --- X20: replace-in-place — any new problem descriptor replaces the held
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
