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
//! - a bind that fails is retried instead of exiting - at a flat second while
//!   the service is merely absent, with backoff once it is answering badly;
//! - an inner trigger that *ends* (portal session closed - `xdg-desktop-portal`
//!   restarted, or the compositor replaced) is rebound rather than treated as
//!   "the user is done", which is what ended the process before.
//!
//! While unbound, the reason is published on `com.canonical.Myna.Dictation.StatusMessage`
//! so the degraded state is inspectable (`gdbus`, the shell extension) rather
//! than only being a line in the journal.

use std::time::Duration;

use async_trait::async_trait;
use gettextrs::gettext;

use super::{Trigger, TriggerEdge};
use crate::dbus::{PropertyValue, SharedBus};

/// First retry delay; doubles per consecutive failure.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Ceiling on the retry delay. A user daemon may sit here for the whole time
/// between PAM login and the compositor coming up, so the retries must stay
/// cheap without becoming so rare that the hotkey is dead for a minute after
/// the portal appears.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Ceiling on the wait while the service is simply not running yet. This is a
/// *safety net*, not the mechanism: [`Rebind::wait_before_retry`] parks on the
/// service appearing and returns the moment it does, so a daemon on a machine
/// with no desktop sleeps rather than polls, and one whose desktop arrives
/// binds in milliseconds. The net only matters if that notification is ever
/// missed, which is why it is finite at all.
const ABSENT_RECHECK: Duration = Duration::from_secs(30);

/// Backstop on one whole bind attempt.
///
/// The bind's own answer deadline lives at the portal boundary
/// ([`crate::shortcut::portal::BIND_TIMEOUT`]), because that is the only place
/// that holds the session it has to hand back on the way out. This is the
/// looser guard around everything else an attempt does - connecting, checking
/// the bus name, installing signal matches - so that a `Rebind` which wedges
/// somewhere this module cannot see still comes back rather than parking the
/// loop forever. (Observed 2026-08-25: restarting `xdg-desktop-portal` under a
/// running daemon left `bind_shortcuts` pending indefinitely.)
///
/// Deliberately longer than the inner deadline: if both are armed the inner
/// one must win, because it is the one that cleans up.
const ATTEMPT_BACKSTOP: Duration = Duration::from_secs(180);

/// Retry delay after a confirm sheet was raised and did not become a binding -
/// declined, or never answered at all.
///
/// This is the *net*, not the mechanism. A sheet that failed is a question
/// already put to whoever is at this machine, and re-putting it on a timer is
/// how an unattended VM ended up with six stacked "Add Keyboard Shortcuts"
/// dialogs (reported 2026-09-01; the dialogs also leaked, which is fixed at
/// the portal boundary). The real signal to ask again is a *new backend* -
/// a portal restart, which in practice means a new desktop session - and
/// [`Rebind::wait_before_retry`] parks on exactly that. The hour is only there
/// so a missed signal heals on its own instead of needing `snap restart myna`.
///
/// Not part of the doubling ladder: it is already the ceiling.
const SHEET_RECHECK: Duration = Duration::from_secs(3600);

/// Recheck interval while no shortcut is bound at all.
///
/// Nothing is wrong and nothing is being waited on: the binding arrives when
/// the user runs `--bind-shortcut`, in another process, and the portal has no
/// signal to offer this one about it. So this is a plain poll - one
/// `ListShortcuts` round trip, no UI - slow enough to cost nothing and quick
/// enough that the hotkey works shortly after being bound.
const UNBOUND_RECHECK: Duration = Duration::from_secs(15);

/// Why a bind attempt failed, and therefore how eagerly to try the next one.
/// These are genuinely different situations and the wrong delay is
/// user-visible either way: too slow and the hotkey is dead after login, too
/// fast and the user is buried in portal dialogs. The dividing line is whether
/// the attempt cost a human anything - the first two ask nobody and can be
/// retried freely, the last two put a dialog on someone's screen and cannot.
#[derive(Debug)]
pub enum BindFailure {
    /// The service is not running and this daemon declined to start it (see
    /// `portal::portal_is_up`). Nothing was asked of anyone, the answer flips
    /// exactly once - when the desktop comes up - and the check is a single
    /// bus round trip, so poll for it at a flat second rather than backing
    /// off away from the moment we are waiting for.
    NotYet(String),
    /// The activation backend was reachable and still could not serve us - a
    /// portal with no GlobalShortcuts implementation, `$XDG_RUNTIME_DIR` not
    /// there yet. Real work for someone else each time, so retry quickly at
    /// first and then back away.
    Unavailable(String),
    /// The request reached the backend and came back without a grant - the
    /// user dismissed the confirm sheet. They have answered, so the next
    /// thing worth asking is a different backend, not them again.
    Refused(String),
    /// A sheet was raised and nobody answered it inside the portal
    /// boundary's deadline - a locked screen, a session switched away from.
    /// Held apart from [`Self::Refused`] because the *diagnosis* differs and
    /// it is user-visible on `StatusMessage`: "the user said no" and "there was
    /// no user" are not the same fault, even though both mean stop asking.
    Unanswered(String),
    /// The portal works and holds no binding for this app. Not degradation -
    /// the daemon is waiting on a step only the user can take, and says so
    /// rather than raising a dialog to ask for it.
    Unbound(String),
}

impl BindFailure {
    fn reason(&self) -> &str {
        match self {
            Self::NotYet(r)
            | Self::Unavailable(r)
            | Self::Refused(r)
            | Self::Unanswered(r)
            | Self::Unbound(r) => r,
        }
    }
}

/// Establishes activation - binds the portal shortcut, opens the control
/// socket - producing a fresh [`Trigger`] each time.
#[async_trait]
pub trait Rebind: Send {
    /// Bind, or explain why not (the message the user sees while degraded).
    async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure>;

    /// Wait before the next attempt, returning early if something makes an
    /// earlier attempt worthwhile.
    ///
    /// Timing out is the fallback, not the design. "The portal is not running"
    /// is an *event* the bus will tell us about, and polling for it is how a
    /// daemon with no desktop to wait for burns CPU forever. Only the rebinder
    /// knows what its own arrival looks like, so the loop delegates the wait
    /// rather than owning a timer; the default is a plain sleep, which is
    /// right for anything with nothing to subscribe to.
    ///
    /// This hook is the loop's *entire* delay between attempts. An
    /// implementation that can fail on its own account - a bus that will not
    /// connect, a signal match that will not install - must still serve out
    /// `delay` before returning, or the retry becomes an unbounded spin at
    /// exactly the moment the system is least able to take one.
    async fn wait_before_retry(&mut self, delay: Duration) {
        tokio::time::sleep(delay).await;
    }
}

/// A [`Trigger`] that binds through a [`Rebind`] factory and survives its
/// backend going away.
pub struct RetryingTrigger {
    rebind: Box<dyn Rebind>,
    inner: Option<Box<dyn Trigger>>,
    backoff: Duration,
    status: Option<SharedBus>,
    /// The failure last reported at the operational tier, so an unchanging one
    /// is stated once instead of every backoff step. A user daemon on a
    /// machine with no compositor never binds, and at the 30 s ceiling that
    /// was ~2,900 identical journal lines a day (measured 2026-08-26 in a
    /// lingering session with no graphical session).
    last_reason: Option<String>,
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
            last_reason: None,
        }
    }

    /// Publish the degraded reason (and its clearing) on
    /// `com.canonical.Myna.Dictation.StatusMessage`.
    pub fn status_on(mut self, bus: SharedBus) -> Self {
        self.status = Some(bus);
        self
    }

    async fn publish(&mut self, message: &str) {
        if let Some(bus) = &self.status {
            bus.lock()
                .await
                .set_property("StatusMessage", PropertyValue::Str(message.to_string()))
                .await;
        }
    }

    /// Block until activation is bound, backing off between attempts. Only
    /// returns having set `self.inner`.
    async fn bind_with_backoff(&mut self) {
        loop {
            // Bound the attempt (see ATTEMPT_BACKSTOP) before matching on it, so
            // the borrow of `self.rebind` ends and `publish` can take `&mut
            // self` below.
            let outcome = match tokio::time::timeout(ATTEMPT_BACKSTOP, self.rebind.bind()).await {
                Ok(result) => result,
                // The backstop fired, so the attempt wedged somewhere the
                // portal boundary's own deadline does not cover. Nothing was
                // answered, so it disposes like an unanswered sheet.
                Err(_) => Err(BindFailure::Unanswered(format!(
                    "bind attempt did not return within {}s",
                    ATTEMPT_BACKSTOP.as_secs()
                ))),
            };
            match outcome {
                Ok(trigger) => {
                    myna_core::info_log!("trigger", "activation bound");
                    self.publish("").await;
                    self.inner = Some(trigger);
                    // Cleared, so a later failure is reported again rather
                    // than being mistaken for one already on record.
                    self.last_reason = None;
                    return;
                }
                Err(failure) => {
                    // The cadence is part of the message because the message
                    // is now printed once: "retrying in 1s" alone, with
                    // nothing after it for hours, reads as a daemon that gave
                    // up after one go.
                    let (delay, cadence, advance) = match failure {
                        BindFailure::NotYet(_) => (
                            ABSENT_RECHECK,
                            "waiting for it to appear".to_string(),
                            false,
                        ),
                        BindFailure::Unavailable(_) => (
                            self.backoff,
                            format!("backing off to every {BACKOFF_MAX:?}"),
                            true,
                        ),
                        // Both mean a sheet was raised and did not take. The
                        // wait is spent parked on a new backend appearing, so
                        // the usual cadence is one sheet per desktop session
                        // rather than one per `SHEET_RECHECK`.
                        BindFailure::Refused(_) | BindFailure::Unanswered(_) => (
                            SHEET_RECHECK,
                            "waiting for a new backend".to_string(),
                            false,
                        ),
                        BindFailure::Unbound(_) => (
                            UNBOUND_RECHECK,
                            "waiting for a shortcut to be bound".to_string(),
                            false,
                        ),
                    };
                    // Said once, in full - including that it keeps trying, so
                    // a single line is not read as "gave up" - and then only
                    // when the reason changes. The unchanging repeats drop to
                    // the debug tier; the current reason stays continuously
                    // readable on `StatusMessage` below, which is the surface
                    // meant for "what is wrong right now" anyway.
                    let reason = failure.reason().to_string();
                    if self.last_reason.as_deref() == Some(reason.as_str()) {
                        myna_core::dbg_log!(
                            "trigger",
                            "still cannot bind activation ({reason}); retrying in {delay:?}"
                        );
                    } else {
                        myna_core::info_log!(
                            "trigger",
                            "cannot bind activation ({reason}); retrying in {delay:?}, {cadence}"
                        );
                        self.last_reason = Some(reason);
                    }
                    self.publish(
                        &gettext("dictation hotkey unavailable: %s")
                            .replace("%s", failure.reason()),
                    )
                    .await;
                    // Under the tests' `start_paused` clock tokio auto-advances
                    // whenever every task is parked on a timer, so the real
                    // backoff sequence runs in no wall-clock time.
                    self.rebind.wait_before_retry(delay).await;
                    // Waiting for a desktop that has not arrived is not
                    // evidence about how the backend behaves, so it leaves the
                    // backoff alone: the first real failure after the portal
                    // shows up still gets the full fast-then-gentle sequence.
                    if advance {
                        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                    }
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
            bus.property("StatusMessage"),
            Some(PropertyValue::Str(
                "dictation hotkey unavailable: global-shortcuts portal unavailable".into()
            ))
        );
    }

    /// Never finds the service running, and is told when one appears - the
    /// daemon on a machine whose desktop has not started (or never will).
    struct AbsentBind {
        attempts: Arc<AtomicUsize>,
        waits: Arc<AtomicUsize>,
        /// Resolves when the "service appeared" event fires.
        appeared: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Rebind for AbsentBind {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(BindFailure::NotYet("nothing owns the bus name".into()))
        }

        async fn wait_before_retry(&mut self, delay: Duration) {
            self.waits.fetch_add(1, Ordering::SeqCst);
            tokio::select! {
                _ = self.appeared.notified() => {}
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    fn absent() -> (RetryingTrigger, Arc<AtomicUsize>, Arc<tokio::sync::Notify>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let appeared = Arc::new(tokio::sync::Notify::new());
        let trigger = RetryingTrigger::new(AbsentBind {
            attempts: Arc::clone(&attempts),
            waits: Arc::new(AtomicUsize::new(0)),
            appeared: Arc::clone(&appeared),
        });
        (trigger, attempts, appeared)
    }

    /// An absent service is an *event*, not a thing to poll for. A daemon on a
    /// machine with no desktop must sleep through the whole wait: polling for
    /// a portal that is never coming was measured at 73 ms of CPU a minute,
    /// ~105 s a day, for nothing.
    #[tokio::test(start_paused = true)]
    async fn an_absent_service_is_waited_on_not_polled() {
        let (mut trigger, attempts, _appeared) = absent();

        let _ = tokio::time::timeout(Duration::from_secs(600), trigger.next_edge()).await;

        // Ten minutes: one attempt per 30 s net, not one per second. The
        // failing shape here is a flat poll, which would be ~600.
        let n = attempts.load(Ordering::SeqCst);
        assert!(
            (19..=22).contains(&n),
            "expected ~20 attempts in 10 min, got {n}"
        );
    }

    /// ...and the point of the event is that the wait *ends* on it. The net is
    /// 30 s; being told at 2 s has to mean binding at 2 s, or the hotkey is
    /// dead for the rest of a minute of a desktop that is plainly working.
    #[tokio::test(start_paused = true)]
    async fn being_told_the_service_appeared_ends_the_wait() {
        let (mut trigger, attempts, appeared) = absent();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            appeared.notify_waiters();
        });
        let _ = tokio::time::timeout(Duration::from_secs(3), trigger.next_edge()).await;

        // t=0 fails, the wait ends at t=2 rather than t=30, so a second
        // attempt happened inside the window.
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the wait did not end when the service appeared"
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

    /// Refuses every bind the way an unattended machine does: the sheet goes
    /// up and nothing comes back.
    struct UnansweredBind {
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Rebind for UnansweredBind {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(BindFailure::Unanswered("no answer within 120s".into()))
        }
    }

    /// Never bound; binds on the attempt after `until`.
    struct UnboundUntil {
        until: usize,
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Rebind for UnboundUntil {
        async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.until {
                return Err(BindFailure::Unbound("no dictation shortcut bound".into()));
            }
            Ok(Box::new(ScriptedTrigger::new(vec![TriggerEdge::Press])))
        }
    }

    /// Nothing bound is not degradation and not a dead end: the user binds in
    /// another process, so this keeps looking - cheaply, and without ever
    /// putting a dialog up to ask.
    #[tokio::test(start_paused = true)]
    async fn an_unbound_shortcut_is_rechecked_until_it_appears() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut trigger = RetryingTrigger::new(UnboundUntil {
            until: 2,
            attempts: Arc::clone(&attempts),
        });

        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    /// and says which command fixes it, rather than reporting a fault.
    #[tokio::test(start_paused = true)]
    async fn an_unbound_shortcut_says_so_on_the_bus() {
        let bus = FakeBus::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let trigger = RetryingTrigger::new(UnboundUntil {
            until: usize::MAX,
            attempts,
        });
        let mut trigger = trigger.status_on(Arc::new(tokio::sync::Mutex::new(bus.clone())));

        let _ = tokio::time::timeout(UNBOUND_RECHECK / 2, trigger.next_edge()).await;

        assert_eq!(
            bus.property("StatusMessage"),
            Some(PropertyValue::Str(
                "dictation hotkey unavailable: no dictation shortcut bound".into()
            ))
        );
    }

    /// A refused bind is a dialog the user just said no to. Retrying it on the
    /// 1s/2s/4s ladder would raise that dialog again immediately and keep
    /// doing it; it has to wait properly instead.
    ///
    /// The window is the whole of `SHEET_RECHECK`, not a couple of minutes:
    /// the point of the policy is that the *only* thing which re-asks inside
    /// that hour is a new backend, and a window shorter than the delay under
    /// test would pass no matter what the delay was.
    #[tokio::test(start_paused = true)]
    async fn a_refused_bind_does_not_re_raise_the_dialog() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut trigger = RetryingTrigger::new(RefusingBind {
            attempts: Arc::clone(&attempts),
        });

        let _ =
            tokio::time::timeout(SHEET_RECHECK - Duration::from_secs(1), trigger.next_edge()).await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the dismissed sheet was put back up inside SHEET_RECHECK"
        );
    }

    /// The reported bug's cadence, at the policy layer.
    ///
    /// A VM left at a lock screen answered nothing for 47 minutes and
    /// collected six sheets: one raised per portal-side bind timeout + the old
    /// five-minute refusal backoff. An unanswered sheet is not a reason to
    /// raise another one - nobody has seen the first yet.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_sheet_is_not_replaced_with_another_one() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut trigger = RetryingTrigger::new(UnansweredBind {
            attempts: Arc::clone(&attempts),
        });

        // The reporter's window, and then some.
        let _ = tokio::time::timeout(Duration::from_secs(47 * 60), trigger.next_edge()).await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "an unattended machine was asked more than once in 47 minutes"
        );
    }

    /// "Nobody answered" and "the user declined" are different diagnoses, and
    /// the one on `StatusMessage` is what a support answer gets written from.
    /// They were the same string until 2026-09-01.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_sheet_reports_itself_as_unanswered() {
        let bus = FakeBus::new();
        let trigger = RetryingTrigger::new(UnansweredBind {
            attempts: Arc::new(AtomicUsize::new(0)),
        });
        let mut trigger = trigger.status_on(Arc::new(tokio::sync::Mutex::new(bus.clone())));

        let _ = tokio::time::timeout(Duration::from_secs(5), trigger.next_edge()).await;

        assert_eq!(
            bus.property("StatusMessage"),
            Some(PropertyValue::Str(
                "dictation hotkey unavailable: no answer within 120s".into()
            ))
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
            bus.property("StatusMessage"),
            Some(PropertyValue::Str(String::new()))
        );
    }
}
