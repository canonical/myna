// tests/states.rs — hermetic contract test for the pure wire-state →
// semantic descriptor mapping (feature 004, contract extension.md RC1–RC4,
// RC6, RC19). No Shell, no D-Bus.
//
use myna_hud::states::{state_to_descriptor, wire, Descriptor, DictationState, Severity};

fn d(state: &str, status_message: &str) -> Descriptor {
    state_to_descriptor(Some(state), status_message)
}

// --- RC1: every known State maps to its semantic descriptor ---------------
// (severity replaces the old boolean isError — RC19)
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
        let desc = state_to_descriptor(Some(state), status_text);
        assert_eq!(desc.key, key, "{state}: key");
        assert_eq!(desc.status_text, status_text, "{state}: statusText");
        assert!(!desc.hidden, "{state}: visible");
        assert_eq!(desc.severity, severity, "{state}: severity");
    }
}

// Status text is publisher-owned, rather than reconstructed by the HUD.
#[test]
fn x1_error_status_message_surfaces_verbatim() {
    let desc = state_to_descriptor(Some(wire::ERROR), "No audio source available");
    assert_eq!(desc.status_text, "No audio source available");
    assert_eq!(desc.severity, Some(Severity::Critical));
}

// Notice keeps recoverable severity while displaying the publisher label.
#[test]
fn x19_notice_with_reason_is_the_status() {
    let desc = state_to_descriptor(Some(wire::NOTICE), "No speech detected");
    assert_eq!(desc.status_text, "No speech detected");
    assert_eq!(desc.severity, Some(Severity::Recoverable));
}

// --- RC2: unknown State → neutral "active" descriptor ----------------------
#[test]
fn x2_unknown_states_map_to_active() {
    for bogus in ["quantizing", "", "RECORDING", "idle "] {
        let desc = d(bogus, "Backend is calibrating");
        assert_eq!(desc.key, DictationState::Active, "{bogus}: key");
        assert_eq!(desc.severity, None, "{bogus}: severity");
        assert!(!desc.hidden, "{bogus}: visible");
        assert_eq!(
            desc.status_text, "Backend is calibrating",
            "{bogus}: statusText"
        );
    }
}

// --- RC3: idle → hidden (no window; push-to-talk) --------------------------
#[test]
fn x3_idle_and_missing_state_are_hidden() {
    assert!(d(wire::IDLE, "").hidden, "idle: hidden");
    assert!(state_to_descriptor(None, "").hidden, "null: hidden");
}

// --- RC4: loading and recording are distinct -------------------------------
#[test]
fn x4_loading_and_recording_are_distinct() {
    let loading = d(wire::LOADING, "Loading model...");
    let recording = d(wire::RECORDING, "Listening");
    assert_ne!(loading.key, recording.key, "key");
    assert_ne!(loading.status_text, recording.status_text, "statusText");
}

// --- RC19: notice and error are mutually exclusive severities --------------
#[test]
fn x19_severities_are_mutually_exclusive() {
    let notice = d(wire::NOTICE, "No speech detected");
    let error = d(wire::ERROR, "Microphone unavailable");
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
        assert_eq!(d(state, "status").severity, None, "{state}: no severity");
    }
}

// --- RC6: descriptor carries only state + content-free status --------------
// The four-field shape is structural in Rust; the behavioral half of RC6 is
// that status text is content-free before the publisher sends it over D-Bus.
#[test]
fn x6_non_problem_status_comes_from_the_publisher() {
    let desc = state_to_descriptor(Some(wire::RECORDING), "Listening");
    assert_eq!(desc.status_text, "Listening");
}
