//! Maps an [`crate::indicator::IndicatorState`] to the announcement text/
//! severity it should produce (contract announcer.md; mirrors
//! `extensions/myna-shell/states.js`'s wording for the non-problem states —
//! FR-024a "identical wording everywhere it is named"). Also produces the
//! accessible name/description pair used for FR-001's on-demand query.
//!
//! Error/notice announcement text is deliberately generic ("Error"/"Notice")
//! for now, not the underlying ad-hoc message: `IndicatorState::Error`'s
//! `message` field is a dynamic backend `String`, not yet the fixed,
//! `&'static str`-typed `FailurePresentation.message` US4 (T067/T068) will
//! introduce. Wiring the *specific* plain-language failure message through
//! to the announcer is T068's job, once `failure::lookup` exists to supply
//! it as a `&'static str` — compatible with [`super::AnnouncementText`]'s
//! compile-time content-free guarantee without an escape hatch.

use crate::indicator::IndicatorState;

use super::{AnnouncementText, Severity};

/// One state's announcement/query text (contracts/announcer.md).
pub struct StateAnnouncement {
    pub name: AnnouncementText,
    pub description: AnnouncementText,
    pub announcement: AnnouncementText,
    pub severity: Option<Severity>,
}

/// The mapping (data-model.md's `DictationState` entity table).
pub fn format_state_announcement(state: &IndicatorState) -> StateAnnouncement {
    match state {
        IndicatorState::Hidden => StateAnnouncement {
            name: AnnouncementText::new("Dictation: idle"),
            description: AnnouncementText::new("No dictation in progress"),
            announcement: AnnouncementText::new("Idle"),
            severity: None,
        },
        IndicatorState::Recording => StateAnnouncement {
            name: AnnouncementText::new("Dictation: listening"),
            description: AnnouncementText::new("Recording your speech"),
            announcement: AnnouncementText::new("Listening"),
            severity: None,
        },
        IndicatorState::Transcribing => StateAnnouncement {
            name: AnnouncementText::new("Dictation: transcribing"),
            description: AnnouncementText::new("Converting your speech to text"),
            announcement: AnnouncementText::new("Transcribing"),
            severity: None,
        },
        IndicatorState::Finalizing => StateAnnouncement {
            name: AnnouncementText::new("Dictation: finishing"),
            description: AnnouncementText::new("Finishing this dictation session"),
            announcement: AnnouncementText::new("Finishing"),
            severity: None,
        },
        IndicatorState::Error {
            recoverable: true, ..
        } => StateAnnouncement {
            name: AnnouncementText::new("Dictation: notice"),
            description: AnnouncementText::new("A recoverable issue occurred"),
            announcement: AnnouncementText::new("Notice"),
            severity: Some(Severity::Recoverable),
        },
        IndicatorState::Error {
            recoverable: false, ..
        } => StateAnnouncement {
            name: AnnouncementText::new("Dictation: error"),
            description: AnnouncementText::new("A failure occurred"),
            announcement: AnnouncementText::new("Error"),
            severity: Some(Severity::Critical),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T034/T038 groundwork: the mapping is total and matches states.js's
    //    wording for the non-problem states (FR-024a) ───────────────────────

    #[test]
    fn every_indicator_state_maps_to_an_announcement() {
        for state in [
            IndicatorState::Hidden,
            IndicatorState::Recording,
            IndicatorState::Transcribing,
            IndicatorState::Finalizing,
            IndicatorState::recoverable("no speech detected"),
            IndicatorState::critical("no microphone"),
        ] {
            let a = format_state_announcement(&state);
            assert!(!a.announcement.as_str().is_empty());
        }
    }

    #[test]
    fn wording_matches_states_js_for_non_problem_states() {
        assert_eq!(
            format_state_announcement(&IndicatorState::Recording)
                .announcement
                .as_str(),
            "Listening"
        );
        assert_eq!(
            format_state_announcement(&IndicatorState::Transcribing)
                .announcement
                .as_str(),
            "Transcribing"
        );
        assert_eq!(
            format_state_announcement(&IndicatorState::Finalizing)
                .announcement
                .as_str(),
            "Finishing"
        );
    }

    #[test]
    fn recoverable_and_critical_errors_carry_the_right_severity() {
        assert_eq!(
            format_state_announcement(&IndicatorState::recoverable("x")).severity,
            Some(Severity::Recoverable)
        );
        assert_eq!(
            format_state_announcement(&IndicatorState::critical("x")).severity,
            Some(Severity::Critical)
        );
        assert_eq!(format_state_announcement(&IndicatorState::Hidden).severity, None);
    }
}
