//! `DbusIndicator` — an [`Indicator`] backend that publishes dictation state
//! onto `org.myna.Dictation` for the GNOME Shell extension (feature 004,
//! contract publisher.md P1–P5). Composes with `NotifyIndicator` as the
//! fallback when the session bus is unavailable (P15). Implementation lands
//! with its hermetic suite (US1/US2).

use crate::indicator::IndicatorState;

/// The `org.myna.Dictation` wire states (data-model E1; the additive string
/// enum the extension switches on).
pub mod wire_state {
    /// No session; goop hidden.
    pub const IDLE: &str = "idle";
    /// Cold model load in progress (the `Loading`-seen / `Ready`-not-yet
    /// window, R4/C5).
    pub const LOADING: &str = "loading";
    /// Capturing / listening (post-`Ready`).
    pub const RECORDING: &str = "recording";
    /// Inference decoding.
    pub const TRANSCRIBING: &str = "transcribing";
    /// Release seen; awaiting the terminal transcript.
    pub const FINALIZING: &str = "finalizing";
    /// Failure / secure-field refusal.
    pub const ERROR: &str = "error";
}

/// Map an [`IndicatorState`] to the `org.myna.Dictation` `State` string
/// (data-model E1 table). `ready_seen` splits `Recording` into the cold-load
/// `loading` window vs post-`Ready` `recording` (R4/C5). Pure and total — the
/// output is always one of the six wire states, so no transcript text can ever
/// cross into the payload through this path (C3).
pub fn map_state(state: &IndicatorState, ready_seen: bool) -> &'static str {
    use wire_state::*;
    match state {
        IndicatorState::Hidden => IDLE,
        IndicatorState::Recording if ready_seen => RECORDING,
        IndicatorState::Recording => LOADING,
        IndicatorState::Transcribing => TRANSCRIBING,
        IndicatorState::Finalizing => FINALIZING,
        IndicatorState::Error(_) => ERROR,
    }
}

/// Session-scoped readiness tracking for the `loading`/`recording` split
/// (R4/P2): the controller maps both `Loading` and `Ready` orchestrator events
/// to `IndicatorState::Recording`, so the publisher keeps whether `Ready` has
/// been seen this session. Reset at each session start.
#[derive(Debug, Default)]
pub struct Readiness {
    ready_seen: bool,
}

impl Readiness {
    /// A fresh, cold session (no `Ready` seen).
    pub fn new() -> Self {
        Self::default()
    }

    /// `Loading` seen — the session is in the cold-load window.
    pub fn note_loading(&mut self) {
        self.ready_seen = false;
    }

    /// `Ready` seen — subsequent `Recording` publishes `recording`.
    pub fn note_ready(&mut self) {
        self.ready_seen = true;
    }

    /// New session: back to cold until the next `Ready`.
    pub fn reset(&mut self) {
        self.ready_seen = false;
    }

    /// Whether `Ready` has been seen this session.
    pub fn ready_seen(&self) -> bool {
        self.ready_seen
    }
}

/// Publishes `IndicatorState` transitions as `StateChanged` signals + `State`
/// property updates via the [`crate::dbus::Bus`] seam. Never carries transcript
/// text (C3).
pub struct DbusIndicator;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicator::IndicatorState;

    /// Contract publisher.md P1 / data-model E1: every IndicatorState maps to
    /// the org.myna.Dictation State string of the E1 table.
    #[test]
    fn indicator_state_maps_to_wire_state() {
        assert_eq!(map_state(&IndicatorState::Hidden, false), "idle");
        assert_eq!(map_state(&IndicatorState::Transcribing, true), "transcribing");
        assert_eq!(map_state(&IndicatorState::Finalizing, true), "finalizing");
        assert_eq!(
            map_state(&IndicatorState::Error("refusing to type".into()), true),
            "error"
        );
    }

    /// R4/C5: the cold-load window — `Recording` seen before `Ready` —
    /// publishes `loading`, and only post-`Ready` `recording`.
    #[test]
    fn recording_splits_on_readiness() {
        assert_eq!(map_state(&IndicatorState::Recording, false), "loading");
        assert_eq!(map_state(&IndicatorState::Recording, true), "recording");
    }

    /// C3: the mapping can only emit one of the six contract state strings —
    /// no payload it produces can carry transcript text.
    #[test]
    fn mapping_outputs_are_content_free() {
        const WIRE_STATES: [&str; 6] = [
            "idle",
            "loading",
            "recording",
            "transcribing",
            "finalizing",
            "error",
        ];
        for ready in [false, true] {
            for state in [
                IndicatorState::Hidden,
                IndicatorState::Recording,
                IndicatorState::Transcribing,
                IndicatorState::Finalizing,
                IndicatorState::Error("a transcript would go here".into()),
            ] {
                assert!(WIRE_STATES.contains(&map_state(&state, ready)));
            }
        }
    }

    /// R4: the tracker resets each session — a warm session (`Ready` seen)
    /// followed by a fresh cold session reports `loading` again until the new
    /// `Ready`.
    #[test]
    fn readiness_tracks_loading_then_ready_per_session() {
        let mut readiness = Readiness::new();
        assert!(!readiness.ready_seen());

        readiness.note_loading();
        assert!(!readiness.ready_seen());
        readiness.note_ready();
        assert!(readiness.ready_seen());

        readiness.reset();
        assert!(!readiness.ready_seen(), "new session starts cold again");
    }
}
