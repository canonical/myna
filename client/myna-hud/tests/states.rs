// tests/states.rs — hermetic contract test for the pure wire-state →
// semantic descriptor mapping (feature 004, contract extension.md X1–X4, X6,
// X19). No Shell, no D-Bus.
//
// English-only assertions: no gettext domain is bound in the test binary,
// so gettext() is the identity function.

use myna_hud::states::{state_to_descriptor, wire, Descriptor, DictationState, Severity};

fn d(state: &str) -> Descriptor {
    state_to_descriptor(Some(state), "")
}

// --- X1: every known State maps to its semantic descriptor ---------------
// (severity replaces the old boolean isError — X19)
#[test]
fn x1_known_states_map_to_their_descriptors() {
    let cases: &[(&str, DictationState, &str, Option<Severity>)] = &[
        (
            wire::LOADING,
            DictationState::Loading,
            "Loading model…",
            None,
        ),
        (
            wire::RECORDING,
            DictationState::Recording,
            "Listening",
            None,
        ),
        (
            wire::TRANSCRIBING,
            DictationState::Transcribing,
            "Transcribing",
            None,
        ),
        (
            wire::FINALIZING,
            DictationState::Finalizing,
            "Finishing",
            None,
        ),
        (
            wire::NOTICE,
            DictationState::Notice,
            "No speech detected",
            Some(Severity::Recoverable),
        ),
        (
            wire::ERROR,
            DictationState::Error,
            "Error",
            Some(Severity::Critical),
        ),
    ];
    for &(state, key, status_text, severity) in cases {
        let desc = state_to_descriptor(Some(state), "");
        assert_eq!(desc.key, key, "{state}: key");
        assert_eq!(desc.status_text, status_text, "{state}: statusText");
        assert!(!desc.hidden, "{state}: visible");
        assert_eq!(desc.severity, severity, "{state}: severity");
    }
}

// Error with a reason surfaces it in the status text (content-free, E3).
#[test]
fn x1_error_with_reason_surfaces_it() {
    let desc = state_to_descriptor(Some(wire::ERROR), "no audio source available");
    assert_eq!(desc.status_text, "Error — no audio source available");
    assert_eq!(desc.severity, Some(Severity::Critical));
}

// Notice with a reason surfaces it directly as the status text (no "Error —"
// prefix — it isn't an error), and is 'recoverable' severity (X19).
#[test]
fn x19_notice_with_reason_is_the_status() {
    let desc = state_to_descriptor(Some(wire::NOTICE), "No speech detected");
    assert_eq!(desc.status_text, "No speech detected");
    assert_eq!(desc.severity, Some(Severity::Recoverable));
}

// --- X2: unknown State → neutral "active" descriptor ----------------------
#[test]
fn x2_unknown_states_map_to_active() {
    for bogus in ["quantizing", "", "RECORDING", "idle "] {
        let desc = d(bogus);
        assert_eq!(desc.key, DictationState::Active, "{bogus}: key");
        assert_eq!(desc.severity, None, "{bogus}: severity");
        assert!(!desc.hidden, "{bogus}: visible");
        assert_eq!(desc.status_text, "Active", "{bogus}: statusText");
    }
}

// --- X3: idle → hidden (no window; push-to-talk) --------------------------
#[test]
fn x3_idle_and_missing_state_are_hidden() {
    assert!(d(wire::IDLE).hidden, "idle: hidden");
    assert!(state_to_descriptor(None, "").hidden, "null: hidden");
}

// --- X4: loading and recording are distinct -------------------------------
#[test]
fn x4_loading_and_recording_are_distinct() {
    let loading = d(wire::LOADING);
    let recording = d(wire::RECORDING);
    assert_ne!(loading.key, recording.key, "key");
    assert_ne!(loading.status_text, recording.status_text, "statusText");
}

// --- X19: notice and error are mutually exclusive severities --------------
#[test]
fn x19_severities_are_mutually_exclusive() {
    let notice = d(wire::NOTICE);
    let error = d(wire::ERROR);
    assert_ne!(notice.key, error.key, "notice ≠ error: key");
    assert_eq!(notice.severity, Some(Severity::Recoverable));
    assert_eq!(error.severity, Some(Severity::Critical));
    assert_ne!(notice.severity, error.severity, "severities differ");
    // Every other known state has no severity.
    for state in [
        wire::LOADING,
        wire::RECORDING,
        wire::TRANSCRIBING,
        wire::FINALIZING,
    ] {
        assert_eq!(d(state).severity, None, "{state}: no severity");
    }
}

// --- X6: descriptor carries only state + content-free status --------------
// The four-field shape is structural in Rust; the behavioral half of X6 is
// that caller text can never flow into a non-problem status.
#[test]
fn x6_non_problem_status_is_fixed() {
    let desc = state_to_descriptor(Some(wire::RECORDING), "a transcript here");
    assert_eq!(desc.status_text, "Listening");
}
