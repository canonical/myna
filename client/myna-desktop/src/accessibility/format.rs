//! Maps an [`crate::indicator::IndicatorState`] to the announcement text/
//! severity it should produce (contract announcer.md; mirrors
//! `extensions/myna-shell/states.js`'s wording for the non-problem states —
//! FR-024a "identical wording everywhere it is named"). Also produces the
//! accessible name/description pair used for FR-001's on-demand query.
//!
//! Error/notice announcement text (US4, T068): when `IndicatorState::Error`
//! carries a `presentation` (built via `IndicatorState::from_failure` from a
//! registered `FailurePresentation`, `crate::failure::lookup`/
//! `lookup_by_code`), the announcement/description use that presentation's
//! own fixed, `&'static str` wording (via `myna_core::failure::spoken`, which
//! combines `message`+`recovery_action` into a single `'static` string —
//! `AnnouncementText`'s compile-time content-free guarantee allows only
//! `&'static str`, never the dynamic backend `String` in `message`).
//! `presentation: None` (a handful of ad-hoc recoverable notices outside
//! `contracts/failure-mapping.md`'s scope — "No speech detected"/"Focus
//! lost") keeps the previous generic "Notice"/"Error" wording.

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
            presentation: Some(p),
            recoverable,
            ..
        } => {
            // T068: the same fixed presentation everywhere it's rendered
            // (F2/F3) — `spoken` combines message+recovery_action into a
            // single `'static` string (see module doc comment); `spoken`
            // always has an entry for a registered `p` (it's populated
            // alongside every `register()` call), but fall back to just
            // `p.message` rather than panicking if that invariant is ever
            // violated by a future registry change.
            let spoken = myna_core::failure::spoken(p.id).unwrap_or(p.message);
            StateAnnouncement {
                name: AnnouncementText::new(if *recoverable {
                    "Dictation: notice"
                } else {
                    "Dictation: error"
                }),
                description: AnnouncementText::new(p.recovery_action),
                announcement: AnnouncementText::new(spoken),
                severity: Some(if *recoverable {
                    Severity::Recoverable
                } else {
                    Severity::Critical
                }),
            }
        }
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
        assert_eq!(
            format_state_announcement(&IndicatorState::Hidden).severity,
            None
        );
    }

    // ── T068: a presentation-backed Error announces the specific fixed
    //    message + recovery action, not the generic "Error"/"Notice" ───────

    #[test]
    fn a_presentation_backed_error_announces_the_specific_message_and_recovery_action() {
        let presentation = myna_core::failure::lookup(myna_core::failure::SECURE_FIELD).unwrap();
        let state = IndicatorState::from_failure(presentation, None);
        let a = format_state_announcement(&state);
        assert!(a.announcement.as_str().contains(presentation.message));
        assert!(a
            .announcement
            .as_str()
            .contains(presentation.recovery_action));
        assert_eq!(a.description.as_str(), presentation.recovery_action);
        assert_eq!(a.severity, Some(Severity::Critical));
    }

    #[test]
    fn a_presentation_backed_recoverable_notice_announces_with_recoverable_severity() {
        let presentation = myna_core::failure::lookup(myna_core::failure::MODEL_LOAD_SLOW).unwrap();
        let state = IndicatorState::from_failure(presentation, None);
        let a = format_state_announcement(&state);
        assert_eq!(a.severity, Some(Severity::Recoverable));
        assert!(a.announcement.as_str().contains(presentation.message));
    }
}
