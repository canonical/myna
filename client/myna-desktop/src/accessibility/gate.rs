//! The announcement gating layer: verbosity filtering (FR-004),
//! burst-coalescing (FR-005), and announcement-failure recovery (FR-002a).
//! Each piece wraps an [`AccessibilityAnnouncer`] and is itself one, so they
//! compose (`CoalescingAnnouncer::new(VerbosityGatedAnnouncer::new(...))`)
//! without `controller.rs` needing to know about any of them individually.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};

use super::{AccessibilityAnnouncer, AnnounceError, AnnouncementText, Severity};
use crate::preferences::{Preferences, Verbosity};

/// Filters `announce()` calls by the current [`Verbosity`] preference
/// (contract A2/A3). `set_state()` always passes through unfiltered — the
/// on-demand accessible query (FR-001) is never affected by verbosity.
pub struct VerbosityGatedAnnouncer<A, P> {
    inner: A,
    preferences: P,
}

impl<A, P> VerbosityGatedAnnouncer<A, P> {
    pub fn new(inner: A, preferences: P) -> Self {
        Self { inner, preferences }
    }
}

#[async_trait]
impl<A, P> AccessibilityAnnouncer for VerbosityGatedAnnouncer<A, P>
where
    A: AccessibilityAnnouncer,
    P: Preferences,
{
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        let allowed = match self.preferences.verbosity() {
            Verbosity::Off => false,
            Verbosity::FailuresOnly => severity.is_some(),
            Verbosity::AllTransitions => true,
        };
        if !allowed {
            return Ok(());
        }
        self.inner.announce(text, severity).await
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        self.inner.set_state(name, description).await;
    }
}

/// One pending announcement awaiting the coalescing window to elapse.
struct Pending {
    text: AnnouncementText,
    severity: Option<Severity>,
}

/// Coalesces a burst of `announce()` calls within `window` into at most one
/// delivered announcement: each new call *resets* the window and replaces
/// the pending announcement, so only the last one in an unbroken burst is
/// ever delivered — earlier ones are dropped entirely, never queued
/// (contract A4, FR-005, SC-003). Implemented as a single debounce actor
/// task (not one spawn per call) so "latest wins" is structural rather than
/// a race between independently-timed tasks. `set_state()` bypasses the
/// actor and updates the inner announcer immediately — FR-001's query must
/// never lag behind the real current state.
pub struct CoalescingAnnouncer<A> {
    inner: Arc<Mutex<A>>,
    tx: mpsc::UnboundedSender<Pending>,
}

impl<A> CoalescingAnnouncer<A>
where
    A: AccessibilityAnnouncer + 'static,
{
    pub fn new(inner: A, window: Duration) -> Self {
        let inner = Arc::new(Mutex::new(inner));
        let actor_inner = inner.clone();
        let (tx, mut rx) = mpsc::unbounded_channel::<Pending>();

        tokio::spawn(async move {
            let mut pending: Option<Pending> = None;
            loop {
                match pending.take() {
                    None => match rx.recv().await {
                        Some(p) => pending = Some(p),
                        None => break,
                    },
                    Some(p) => {
                        tokio::select! {
                            biased;
                            next = rx.recv() => match next {
                                // A newer call arrived before the window
                                // elapsed: it replaces `p`, which is dropped
                                // here without ever reaching `inner`.
                                Some(newer) => pending = Some(newer),
                                None => break,
                            },
                            _ = tokio::time::sleep(window) => {
                                let mut inner = actor_inner.lock().await;
                                let _ = inner.announce(p.text, p.severity).await;
                            }
                        }
                    }
                }
            }
        });

        Self { inner, tx }
    }
}

#[async_trait]
impl<A> AccessibilityAnnouncer for CoalescingAnnouncer<A>
where
    A: AccessibilityAnnouncer + 'static,
{
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        self.tx
            .send(Pending { text, severity })
            .map_err(|_| AnnounceError("coalescing actor task has stopped".to_string()))
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        let mut inner = self.inner.lock().await;
        inner.set_state(name, description).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::FakeAnnouncer;
    use crate::preferences::DefaultPreferences;

    // ── T013/T014: verbosity gating ─────────────────────────────────────────

    struct FixedVerbosity(Verbosity);

    impl Preferences for FixedVerbosity {
        fn verbosity(&self) -> Verbosity {
            self.0
        }
        fn sound_cues_enabled(&self) -> bool {
            DefaultPreferences.sound_cues_enabled()
        }
        fn silence_auto_stop_seconds(&self) -> Option<u32> {
            DefaultPreferences.silence_auto_stop_seconds()
        }
    }

    #[tokio::test]
    async fn off_suppresses_announce_but_not_set_state() {
        let fake = FakeAnnouncer::new();
        let mut gated = VerbosityGatedAnnouncer::new(fake, FixedVerbosity(Verbosity::Off));

        gated
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        gated
            .set_state(
                AnnouncementText::new("Dictation: listening"),
                AnnouncementText::new("Recording"),
            )
            .await;

        assert_eq!(
            gated.inner.calls.len(),
            1,
            "only set_state should reach the inner announcer"
        );
    }

    #[tokio::test]
    async fn failures_only_suppresses_transitions_but_not_failures() {
        let fake = FakeAnnouncer::new();
        let mut gated =
            VerbosityGatedAnnouncer::new(fake, FixedVerbosity(Verbosity::FailuresOnly));

        gated
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        gated
            .announce(AnnouncementText::new("error"), Some(Severity::Critical))
            .await
            .unwrap();

        assert_eq!(
            gated.inner.calls.len(),
            1,
            "only the severity-bearing announcement should pass"
        );
    }

    #[tokio::test]
    async fn all_transitions_passes_everything() {
        let fake = FakeAnnouncer::new();
        let mut gated =
            VerbosityGatedAnnouncer::new(fake, FixedVerbosity(Verbosity::AllTransitions));

        gated
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        gated
            .announce(AnnouncementText::new("error"), Some(Severity::Critical))
            .await
            .unwrap();

        assert_eq!(gated.inner.calls.len(), 2);
    }

    // ── T016: coalescing drops superseded announcements within the window ───

    #[tokio::test(start_paused = true)]
    async fn a_burst_within_the_window_delivers_only_the_last() {
        let fake = FakeAnnouncer::new();
        let window = Duration::from_millis(50);
        let mut coalescing = CoalescingAnnouncer::new(fake, window);
        let inner_handle = coalescing.inner.clone();

        coalescing
            .announce(AnnouncementText::new("loading"), None)
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;

        coalescing
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;

        coalescing
            .announce(AnnouncementText::new("transcribing"), None)
            .await
            .unwrap();
        tokio::task::yield_now().await;

        // Let the final pending announcement's window fully elapse.
        tokio::time::advance(window + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        let calls = inner_handle.lock().await.calls.clone();
        assert_eq!(
            calls,
            vec![crate::accessibility::fake::Recorded::Announce {
                text: "transcribing".to_string(),
                severity: None,
            }],
            "only the final, non-superseded announcement in the burst should be delivered"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn announcements_spaced_beyond_the_window_are_all_delivered() {
        let fake = FakeAnnouncer::new();
        let window = Duration::from_millis(50);
        let mut coalescing = CoalescingAnnouncer::new(fake, window);
        let inner_handle = coalescing.inner.clone();

        coalescing
            .announce(AnnouncementText::new("loading"), None)
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(window + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        coalescing
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(window + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        let calls = inner_handle.lock().await.calls.clone();
        assert_eq!(calls.len(), 2, "no coalescing across separate windows");
    }
}

