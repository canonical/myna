//! Announcement-failure recovery (contract A5, FR-002a): wraps an
//! `AccessibilityAnnouncer` so that a failed `announce()` call never
//! propagates as a session-aborting error — the caller always gets `Ok(())`
//! back, and the failure is instead recorded as a `Recoverable`
//! `FailurePresentation` the controller can surface through the remaining
//! working channels (never silently swallowed).

use async_trait::async_trait;

use super::{AccessibilityAnnouncer, AnnounceError, AnnouncementText, Severity};
use crate::failure::FailurePresentation;

const ANNOUNCEMENT_FAILED: FailurePresentation = FailurePresentation {
    id: "accessibility_announcement_failed",
    message: "A screen-reader announcement could not be sent.",
    recovery_action: "Dictation continues normally; check your accessibility settings if this keeps happening.",
    severity: Severity::Recoverable,
};

pub struct RecoveringAnnouncer<A> {
    inner: A,
    last_notice: Option<FailurePresentation>,
}

impl<A> RecoveringAnnouncer<A> {
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            last_notice: None,
        }
    }

    /// Returns and clears the most recent announcement-failure notice, if
    /// any (FR-026: still discoverable after the fact, not only in the
    /// instant it occurred).
    pub fn take_recovery_notice(&mut self) -> Option<FailurePresentation> {
        self.last_notice.take()
    }
}

#[async_trait]
impl<A> AccessibilityAnnouncer for RecoveringAnnouncer<A>
where
    A: AccessibilityAnnouncer,
{
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        if self.inner.announce(text, severity).await.is_err() {
            self.last_notice = Some(ANNOUNCEMENT_FAILED);
        }
        // The dictation session MUST continue unaffected (FR-002a): the
        // failure is recorded above, never propagated to the caller.
        Ok(())
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        self.inner.set_state(name, description).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::FakeAnnouncer;

    // ── T018: announce() failure never propagates; becomes a Recoverable
    //          FailurePresentation instead of being swallowed (A5, FR-002a) ─

    #[tokio::test]
    async fn a_failed_announce_returns_ok_and_records_a_recoverable_notice() {
        let mut fake = FakeAnnouncer::new();
        fake.fail_next = Some(AnnounceError("bus unreachable".to_string()));
        let mut recovering = RecoveringAnnouncer::new(fake);

        let result = recovering
            .announce(AnnouncementText::new("listening"), None)
            .await;

        assert!(
            result.is_ok(),
            "the caller's session flow must continue unaffected"
        );
        let notice = recovering.take_recovery_notice();
        assert_eq!(notice, Some(ANNOUNCEMENT_FAILED));
        assert_eq!(notice.unwrap().severity, Severity::Recoverable);
    }

    #[tokio::test]
    async fn a_successful_announce_records_no_notice() {
        let fake = FakeAnnouncer::new();
        let mut recovering = RecoveringAnnouncer::new(fake);

        recovering
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();

        assert_eq!(recovering.take_recovery_notice(), None);
    }

    #[tokio::test]
    async fn the_notice_is_still_retrievable_after_the_call_returns() {
        // FR-026: a transient failure must not be the only record — asserted
        // here as "the notice remains queryable until explicitly taken",
        // not just visible synchronously inside the failing call.
        let mut fake = FakeAnnouncer::new();
        fake.fail_next = Some(AnnounceError("bus unreachable".to_string()));
        let mut recovering = RecoveringAnnouncer::new(fake);

        recovering
            .announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        // Some further, unrelated work happens here in real usage...
        assert!(recovering.take_recovery_notice().is_some());
        // ...and once taken, it is consumed (not re-delivered indefinitely).
        assert_eq!(recovering.take_recovery_notice(), None);
    }
}
