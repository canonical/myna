//! `RetryingTrigger` - keep trying to hold the hotkey, forever.
//!
//! Binding activation is the one startup step that genuinely cannot be done
//! lazily: the controller is parked in `next_edge()` waiting to be told the
//! user pressed the key, so if nothing is bound, nothing ever happens. It is
//! also the step most likely to fail at the moment a daemon starts -
//! `xdg-desktop-portal` is socket-activated and its GlobalShortcuts backend
//! belongs to the compositor, so on a user daemon started at PAM login the
//! portal is simply not there yet.
//!
//! Both halves of that are handled here, and they are the same fix:
//!
//! - a bind that fails is retried with backoff instead of exiting;
//! - an inner trigger that *ends* (portal session closed - `xdg-desktop-portal`
//!   restarted, or the compositor replaced) is rebound rather than treated as
//!   "the user is done", which is what ended the process before.
//!
//! While unbound, the reason is published on `org.myna.Dictation.ErrorMessage`
//! so the degraded state is inspectable (`gdbus`, the shell extension) rather
//! than only being a line in the journal.

use std::time::Duration;

use async_trait::async_trait;

use super::{Trigger, TriggerEdge};
use crate::dbus::{PropertyValue, SharedBus};

/// First retry delay; doubles per consecutive failure.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Ceiling on the retry delay. A user daemon may sit here for the whole time
/// between PAM login and the compositor coming up, so the retries must stay
/// cheap without becoming so rare that the hotkey is dead for a minute after
/// the portal appears.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long one bind attempt may take before it is abandoned and retried.
///
/// The portal's `BindShortcuts` resolves on a `Response` *signal*, not on the
/// method reply, so no D-Bus call timeout applies to it and a portal that
/// never answers never returns. An unbounded await here defeats the whole
/// point of this type: the loop cannot retry what it is still waiting on, so
/// the daemon stays up, keeps the bus name, logs nothing further and is deaf
/// until something restarts it - strictly worse than the failure the retrying
/// exists to fix. (Observed 2026-08-25: restarting `xdg-desktop-portal` under
/// a running daemon left `bind_shortcuts` pending indefinitely.)
///
/// Generous rather than tight, because a bind can legitimately block on a
/// user-facing confirm sheet: portal v1 has no persist token, so backends
/// older than xdg-desktop-portal-gnome 51 prompt once per bind. Timing out
/// under someone's cursor would just raise a second sheet. What matters is
/// that it is finite.
const BIND_TIMEOUT: Duration = Duration::from_secs(120);

/// Retry delay after the backend had the request and did not grant it. Long,
/// and deliberately not part of the doubling sequence: the portal's bind is a
/// *user-facing confirm sheet* (portal v1 has no persist token, so there is
/// one per bind), and retrying a refused or ignored sheet on the fast backoff
/// re-raises that dialog every second or two. Still finite, so a sheet
/// dismissed by accident heals on its own instead of needing
/// `snap restart myna`.
const REFUSED_BACKOFF: Duration = Duration::from_secs(300);

/// Why a bind attempt failed, and therefore how eagerly to try the next one.
/// The two are genuinely different situations and the wrong delay is
/// user-visible either way: too slow and the hotkey is dead after login, too
/// fast and the user is buried in portal dialogs.
#[derive(Debug)]
pub enum BindFailure {
    /// The activation backend could not be reached at all - the portal is not
    /// up yet, `$XDG_RUNTIME_DIR` does not exist yet. This is the PAM-login
    /// race and nobody is being interrupted, so retry quickly.
    Unavailable(String),
    /// The request reached the backend and came back without a grant - the
    /// user dismissed the confirm sheet, or never answered it. Retrying fast
    /// would just raise the sheet again.
    Refused(String),
}

impl BindFailure {
    fn reason(&self) -> &str {
        match self {
            Self::Unavailable(r) | Self::Refused(r) => r,
        }
    }
}

/// Establishes activation - binds the portal shortcut, opens the control
/// socket - producing a fresh [`Trigger`] each time.
#[async_trait]
pub trait Rebind: Send {
    /// Bind, or explain why not (the message the user sees while degraded).
    async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure>;
}

/// A [`Trigger`] that binds through a [`Rebind`] factory and survives its
/// backend going away.
pub struct RetryingTrigger {
    rebind: Box<dyn Rebind>,
    inner: Option<Box<dyn Trigger>>,
    backoff: Duration,
    status: Option<SharedBus>,
}

impl RetryingTrigger {
    /// Wrap a bind factory. Nothing is bound until the first
    /// [`Trigger::next_edge`], which is where the controller parks anyway.
    pub fn new(rebind: impl Rebind + 'static) -> Self {
        Self {
            rebind: Box::new(rebind),
            inner: None,
            backoff: BACKOFF_START,
            status: None,
        }
    }

    /// Publish the degraded reason (and its clearing) on
    /// `org.myna.Dictation.ErrorMessage`.
    pub fn status_on(mut self, bus: SharedBus) -> Self {
        self.status = Some(bus);
        self
    }

    async fn publish(&mut self, message: &str) {
        if let Some(bus) = &self.status {
            bus.lock()
                .await
                .set_property("ErrorMessage", PropertyValue::Str(message.to_string()))
                .await;
        }
    }

    /// Block until activation is bound, backing off between attempts. Only
    /// returns having set `self.inner`.
    async fn bind_with_backoff(&mut self) {
        loop {
            // Bound the attempt (see BIND_TIMEOUT) before matching on it, so
            // the borrow of `self.rebind` ends and `publish` can take `&mut
            // self` below.
            let outcome = match tokio::time::timeout(BIND_TIMEOUT, self.rebind.bind()).await {
                Ok(result) => result,
                // Timing out means the backend took the request and sat on it,
                // which is a sheet nobody answered - the same disposition as a
                // refusal, not a reason to hammer.
                Err(_) => Err(BindFailure::Refused(format!(
                    "no answer within {}s (an unanswered shortcut dialog?)",
                    BIND_TIMEOUT.as_secs()
                ))),
            };
            match outcome {
                Ok(trigger) => {
                    myna_core::info_log!("trigger", "activation bound");
                    self.publish("").await;
                    self.inner = Some(trigger);
                    return;
                }
                Err(failure) => {
                    let delay = match failure {
                        BindFailure::Unavailable(_) => self.backoff,
                        BindFailure::Refused(_) => REFUSED_BACKOFF,
                    };
                    myna_core::info_log!(
                        "trigger",
                        "cannot bind activation ({}); retrying in {delay:?}",
                        failure.reason()
                    );
                    self.publish(&format!(
                        "dictation hotkey unavailable: {}",
                        failure.reason()
                    ))
                    .await;
                    // Under the tests' `start_paused` clock tokio auto-advances
                    // whenever every task is parked on a timer, so the real
                    // backoff sequence runs in no wall-clock time.
                    tokio::time::sleep(delay).await;
                    self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }
}

#[async_trait]
impl Trigger for RetryingTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        loop {
            if self.inner.is_none() {
                self.bind_with_backoff().await;
            }
            match self
                .inner
                .as_mut()
                .expect("bound just above")
                .next_edge()
                .await
            {
                Some(edge) => {
                    // A delivered edge is the only proof the binding actually
                    // works; resetting here (rather than on a successful
                    // bind) keeps a portal that accepts the bind and then
                    // immediately drops the session from being rebound in a
                    // tight loop.
                    self.backoff = BACKOFF_START;
                    return Some(edge);
                }
                None => {
                    myna_core::info_log!("trigger", "activation ended; rebinding");
                    self.inner = None;
                }
            }
        }
    }

    async fn discard_pending(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.discard_pending().await;
        }
    }

    async fn resync(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.resync().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::FakeBus;
    use myna_orchestrator::ScriptedTrigger;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Fails the first `fails` binds, then hands out a trigger scripted with
    /// `edges` - a fresh one per bind, so a rebind produces new edges.
    struct FlakyBind {
        fails: usize,
        edges: Vec<TriggerEdge>,
        attempts: Arc<AtomicUsize>,
    }

    /// Hangs forever on the first bind, then succeeds - the portal that
    /// accepts a request and never answers it.
    struct HangingBind {
        hung: bool,
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Rebind for HangingBind {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if !self.hung {
                self.hung = true;
                std::future::pending::<()>().await;
            }
            Ok(Box::new(ScriptedTrigger::new([TriggerEdge::Press])))
        }
    }

    #[async_trait]
    impl Rebind for FlakyBind {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.fails {
                return Err(BindFailure::Unavailable(
                    "global-shortcuts portal unavailable".into(),
                ));
            }
            Ok(Box::new(ScriptedTrigger::new(self.edges.clone())))
        }
    }

    fn flaky(fails: usize, edges: Vec<TriggerEdge>) -> (RetryingTrigger, Arc<AtomicUsize>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let trigger = RetryingTrigger::new(FlakyBind {
            fails,
            edges,
            attempts: Arc::clone(&attempts),
        });
        (trigger, attempts)
    }

    /// The startup case a user daemon actually hits: the portal is not up when
    /// the process starts. The old code exited here; now the first press just
    /// arrives once the portal appears.
    #[tokio::test(start_paused = true)]
    async fn a_portal_that_is_not_up_yet_is_waited_out() {
        let (mut trigger, attempts) = flaky(3, vec![TriggerEdge::Press]);

        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            4,
            "three failures then the bind that worked"
        );
    }

    /// `xdg-desktop-portal` restarting ends the portal session, which the
    /// inner trigger reports as `None`. That must not end the daemon.
    #[tokio::test(start_paused = true)]
    async fn the_backend_going_away_rebinds_instead_of_quitting() {
        let (mut trigger, attempts) = flaky(0, vec![TriggerEdge::Press]);

        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        // The scripted trigger is exhausted → `None` → rebind → a fresh one.
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// While unbound, the reason is on the bus - otherwise "the hotkey does
    /// nothing" is only visible in the journal.
    #[tokio::test(start_paused = true)]
    async fn the_degraded_reason_is_published_while_unbound() {
        let bus = FakeBus::new();
        let (trigger, _) = flaky(usize::MAX, vec![]);
        let mut trigger = trigger.status_on(Arc::new(tokio::sync::Mutex::new(bus.clone())));

        // Never binds, so `next_edge` never returns; sample it mid-retry.
        let _ = tokio::time::timeout(Duration::from_secs(5), trigger.next_edge()).await;

        assert_eq!(
            bus.property("ErrorMessage"),
            Some(PropertyValue::Str(
                "dictation hotkey unavailable: global-shortcuts portal unavailable".into()
            ))
        );
    }

    /// Refuses every bind - the user dismissing the portal's confirm sheet.
    struct RefusingBind {
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Rebind for RefusingBind {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(BindFailure::Refused("shortcut bind rejected".into()))
        }
    }

    /// A refused bind is a dialog the user just said no to. Retrying it on the
    /// 1s/2s/4s ladder would raise that dialog again immediately and keep
    /// doing it; it has to wait properly instead.
    #[tokio::test(start_paused = true)]
    async fn a_refused_bind_does_not_re_raise_the_dialog() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut trigger = RetryingTrigger::new(RefusingBind {
            attempts: Arc::clone(&attempts),
        });

        // Two minutes is already past several rungs of the fast backoff.
        let _ = tokio::time::timeout(Duration::from_secs(120), trigger.next_edge()).await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "no second dialog within the refusal backoff"
        );
    }

    /// A bind that never answers must not park the retry loop. The portal
    /// resolves `BindShortcuts` on a signal, so nothing underneath imposes a
    /// timeout; without this the daemon stays up and deaf forever, which is
    /// how it behaved against a restarted `xdg-desktop-portal` on 2026-08-25.
    #[tokio::test(start_paused = true)]
    async fn a_bind_that_never_answers_is_abandoned_and_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut trigger = RetryingTrigger::new(HangingBind {
            hung: false,
            attempts: Arc::clone(&attempts),
        });

        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the hung attempt timed out and the next one bound"
        );
    }

    /// and clears once the hotkey is actually held, so a stale reason does
    /// not outlive the condition.
    #[tokio::test(start_paused = true)]
    async fn the_degraded_reason_clears_once_bound() {
        let bus = FakeBus::new();
        let (trigger, _) = flaky(1, vec![TriggerEdge::Press]);
        let mut trigger = trigger.status_on(Arc::new(tokio::sync::Mutex::new(bus.clone())));

        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(
            bus.property("ErrorMessage"),
            Some(PropertyValue::Str(String::new()))
        );
    }
}
