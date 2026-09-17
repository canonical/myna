//! `AnnouncingIndicator` — an [`Indicator`] wrapper that drives an
//! [`AccessibilityAnnouncer`] alongside the wrapped indicator on every
//! transition (US1, T036; plan.md Structure Decision: "a single
//! `controller.rs` transition can drive both without either seam knowing
//! about the other"). `controller.rs` itself is unchanged — every existing
//! `Indicator::set_state()`/`hide()` call site (there are several, including
//! free functions like `report_critical`/`enter_finalizing` that take
//! `&mut dyn Indicator`) transparently gains an announcement the moment the
//! concrete indicator handed to [`crate::controller::DesktopControllerBuilder::indicator`]
//! is wrapped in this type.

use async_trait::async_trait;

use super::format::format_state_announcement;
use super::AccessibilityAnnouncer;
use crate::indicator::{Indicator, IndicatorState};

/// A critical failure — one the user must acknowledge, as opposed to a
/// recoverable notice that clears itself.
///
/// Exempt from the dedup below (FR-024/FR-025). The dedup exists for
/// `controller.rs`'s deliberate double-call on the completed-session state,
/// and `completion_indicator_state` only ever yields recoverable notices or
/// `Hidden` — so no critical failure is ever published twice by accident.
/// When one *is*, the user has retried and hit the same wall, and being told
/// nothing the second time reads as the key having done nothing at all.
fn is_critical(state: &IndicatorState) -> bool {
    matches!(
        state,
        IndicatorState::Error {
            recoverable: false,
            ..
        }
    )
}

/// Wraps a visual [`Indicator`] and a non-visual [`AccessibilityAnnouncer`];
/// implements `Indicator` itself so it drops into any seam that already
/// expects one — no other code needs to change.
///
/// Deduplicates consecutive identical states before announcing (never before
/// forwarding to the wrapped `Indicator`, whose own dedup — if any — is
/// unaffected): `controller.rs` already calls `set_state` twice for the same
/// completed-session state by design (the live per-event path and the
/// finalize-block safety net, so the two "can never disagree" — see
/// `completion_indicator_state`'s doc comment), relying on
/// `DbusIndicator::publish`'s own per-wire-state dedup to make the second
/// call a no-op for that indicator. `MockIndicator`/announcers have no such
/// built-in dedup, so this wrapper provides the same idempotency generically
/// (contract A4's "at most one current announcement", applied to the
/// zero-state-change case, not only a burst of different states).
pub struct AnnouncingIndicator<I, A> {
    indicator: I,
    announcer: A,
    last_announced: Option<IndicatorState>,
}

impl<I, A> AnnouncingIndicator<I, A> {
    pub fn new(indicator: I, announcer: A) -> Self {
        Self {
            indicator,
            announcer,
            last_announced: None,
        }
    }
}

#[async_trait]
impl<I, A> Indicator for AnnouncingIndicator<I, A>
where
    I: Indicator,
    A: AccessibilityAnnouncer,
{
    async fn set_state(&mut self, state: IndicatorState) {
        // Visual first, matching the order every existing call site already
        // used before this wrapper existed (no change to the visual timing
        // contract) — non-visual is additive, not replacing.
        self.indicator.set_state(state.clone()).await;
        if self.last_announced.as_ref() == Some(&state) && !is_critical(&state) {
            return;
        }
        self.last_announced = Some(state.clone());
        let a = format_state_announcement(&state);
        self.announcer.set_state(a.name, a.description).await;
        // A failed announce() must not surface here (FR-002a): the
        // `RecoveringAnnouncer` layer (Foundational phase) already converts
        // failures into a recoverable notice rather than an `Err` a caller
        // could see; a bare `AccessibilityAnnouncer` (e.g. in a test) that
        // returns `Err` directly is simply ignored — this wrapper's own
        // contract is "never propagate," matching FR-002a at every layer.
        let _ = self.announcer.announce(a.announcement, a.severity).await;
    }

    async fn hide(&mut self) {
        self.indicator.hide().await;
        if self.last_announced.as_ref() == Some(&IndicatorState::Hidden) {
            return;
        }
        self.last_announced = Some(IndicatorState::Hidden);
        let a = format_state_announcement(&IndicatorState::Hidden);
        self.announcer.set_state(a.name, a.description).await;
        let _ = self.announcer.announce(a.announcement, a.severity).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::fake::Recorded;
    use crate::accessibility::FakeAnnouncer;
    use crate::indicator::mock::MockIndicator;

    // ── T030 (unit-level): every set_state()/hide() call drives both the
    //    wrapped indicator AND the announcer, exactly once each ────────────

    #[tokio::test]
    async fn set_state_drives_both_the_indicator_and_the_announcer() {
        let mock = MockIndicator::new();
        let mock_log = mock.log();
        let fake = FakeAnnouncer::new();
        let announcer_log = fake.log();
        let mut wrapped = AnnouncingIndicator::new(mock, fake);

        wrapped.set_state(IndicatorState::Recording).await;

        assert_eq!(*mock_log.lock().unwrap(), vec![IndicatorState::Recording]);
        let calls = announcer_log.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                Recorded::SetState {
                    name: "Dictation: listening".to_string(),
                    description: "Recording your speech".to_string(),
                },
                Recorded::Announce {
                    text: "Listening".to_string(),
                    severity: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn hide_drives_both_the_indicator_and_the_announcer() {
        let mock = MockIndicator::new();
        let mock_log = mock.log();
        let fake = FakeAnnouncer::new();
        let announcer_log = fake.log();
        let mut wrapped = AnnouncingIndicator::new(mock, fake);

        wrapped.hide().await;

        assert_eq!(*mock_log.lock().unwrap(), vec![IndicatorState::Hidden]);
        let calls = announcer_log.lock().unwrap().clone();
        assert!(calls
            .iter()
            .any(|c| matches!(c, Recorded::Announce { text, .. } if text == "Idle")));
    }

    #[tokio::test]
    async fn a_full_transition_walk_announces_each_state_once() {
        let mock = MockIndicator::new();
        let fake = FakeAnnouncer::new();
        let announcer_log = fake.log();
        let mut wrapped = AnnouncingIndicator::new(mock, fake);

        for state in [
            IndicatorState::Recording,
            IndicatorState::Transcribing,
            IndicatorState::Finalizing,
        ] {
            wrapped.set_state(state).await;
        }
        wrapped.hide().await;

        let announce_texts: Vec<String> = announcer_log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                Recorded::Announce { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            announce_texts,
            vec!["Listening", "Transcribing", "Finishing", "Idle"],
            "loading→listening→transcribing→finishing→idle: each included transition announced exactly once (Acceptance Scenario 2)"
        );
    }

    // ── FR-024/FR-025: a repeated critical failure is repeated news ───────

    fn announce_texts(log: &std::sync::Arc<std::sync::Mutex<Vec<Recorded>>>) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                Recorded::Announce { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Retrying into the same wall — the microphone is still missing, the
    /// field is still protected — is the user's action failing again, not a
    /// state that has not changed. `controller.rs` publishes the failure
    /// with no intervening state on the pre-capture abort path, so without
    /// this the second attempt is answered with complete silence.
    #[tokio::test]
    async fn the_same_critical_failure_is_announced_again_each_time_it_happens() {
        let fake = FakeAnnouncer::new();
        let log = fake.log();
        let mut wrapped = AnnouncingIndicator::new(MockIndicator::new(), fake);

        let failure = IndicatorState::critical("No microphone available");
        wrapped.set_state(failure.clone()).await;
        wrapped.set_state(failure).await;

        assert_eq!(
            announce_texts(&log).len(),
            2,
            "a repeated critical failure must be announced again, not swallowed"
        );
    }

    /// The dedup's original subject is untouched: `controller.rs` calls
    /// `set_state` twice for the same completed-session state by design (the
    /// live per-event path and the finalize-block safety net), and those
    /// states are always recoverable.
    #[tokio::test]
    async fn a_repeated_recoverable_notice_is_still_announced_only_once() {
        let fake = FakeAnnouncer::new();
        let log = fake.log();
        let mut wrapped = AnnouncingIndicator::new(MockIndicator::new(), fake);

        let notice = IndicatorState::recoverable("No speech detected");
        wrapped.set_state(notice.clone()).await;
        wrapped.set_state(notice).await;

        assert_eq!(announce_texts(&log).len(), 1);
    }
}
