//! `MockInjector` — the hermetic text-injection fixture (T007).
//!
//! Scripts `acquire` outcomes and focus loss, and records every `commit` /
//! `set_preedit` / `release` so controller tests can assert commit order/count,
//! teardown, and the commit-only invariant - with no IBus, D-Bus, or display.
//! Its targets hold a lease like `IbusInjector`'s: focus loss is retained, and
//! output after it is refused (but still recorded, so a test sees the
//! controller attempt it). A loss scripted with
//! [`MockInjector::with_focus_event`] is delivered through the target's focus
//! stream, so a controller that fails to act on the event still holds a live
//! target and writes into it. `supports_preedit()` is `false` by default
//! (commit-only); preedit tests opt in via [`MockInjector::with_preedit_support`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::watch;

use super::{FocusEvent, InjectError, Injector, Target};

/// A recording of what the controller did to the injector. Shared with the test
/// via [`MockInjector::log`] so assertions survive the controller owning the
/// injector.
#[derive(Debug, Default)]
pub struct InjectorLog {
    /// Attempted commits, in order (commit-only invariant), including those
    /// the lease refused.
    pub commits: Vec<String>,
    /// Preedit texts passed to `set_preedit`, in order (volatile — must never
    /// also appear in `commits`), including those the lease refused.
    pub preedits: Vec<String>,
    /// Interleaved commit/preedit operation order (`"commit"` / `"preedit"`),
    /// so tests can assert a pending commit always lands *before* the preedit
    /// tail that follows it.
    pub order: Vec<&'static str>,
    /// Number of `acquire` calls.
    pub acquires: usize,
    /// Number of targets released, each restoring the prior engine once (I11).
    pub releases: usize,
}

/// The outcome a scripted `acquire()` yields.
#[derive(Debug, Clone)]
pub enum AcquireOutcome {
    /// Bind a normal editable target.
    Ok,
    /// A password/secure field — `Err(SecureField)`.
    Secure,
    /// Nothing editable focused — `Err(NoTarget)`.
    NoTarget,
    /// Backend unreachable — `Err(Unavailable(msg))`.
    Unavailable(String),
}

/// The mock's lease: which target may write, and why it may not any more.
#[derive(Debug, Clone, Copy)]
struct Lease {
    id: u64,
    lost: Option<FocusEvent>,
}

impl Lease {
    /// Why target `id` may not write, or `None` while it may.
    fn loss(&self, id: u64) -> Option<FocusEvent> {
        if self.id == id {
            self.lost
        } else {
            Some(FocusEvent::FocusOut)
        }
    }
}

/// Delivers focus loss to the target the mock handed out last, at a moment
/// the test chooses.
#[derive(Clone, Debug)]
pub struct FocusSender(Arc<watch::Sender<Lease>>);

impl FocusSender {
    pub fn send(&self, event: FocusEvent) {
        self.0.send_modify(|lease| {
            lease.lost.get_or_insert(event);
        });
    }
}

/// A hermetic [`Injector`] driven by a script. Clone the [`InjectorLog`] handle
/// (`.log()`) *before* moving the mock into the controller to read it afterward.
pub struct MockInjector {
    acquires: VecDeque<AcquireOutcome>,
    /// Focus lost as each target's focus stream is polled.
    focus: Option<FocusEvent>,
    /// Focus lost while `acquire` runs.
    focus_during_acquire: Option<FocusEvent>,
    /// Focus lost while a target's first `commit` is in flight.
    focus_during_commit: Option<FocusEvent>,
    lease: Arc<watch::Sender<Lease>>,
    /// What `supports_preedit()` reports (false unless opted in).
    preedit_supported: bool,
    log: Arc<Mutex<InjectorLog>>,
}

impl Default for MockInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl MockInjector {
    /// A mock whose first `acquire` succeeds with a default target and which
    /// never loses focus.
    pub fn new() -> Self {
        Self {
            acquires: VecDeque::from([AcquireOutcome::Ok]),
            focus: None,
            focus_during_acquire: None,
            focus_during_commit: None,
            lease: Arc::new(watch::Sender::new(Lease {
                id: 0,
                lost: Some(FocusEvent::FocusOut),
            })),
            preedit_supported: false,
            log: Arc::new(Mutex::new(InjectorLog::default())),
        }
    }

    /// Report a replacement-safe preedit region and record `set_preedit` calls
    /// (the IBus backend's behavior; default is commit-only).
    pub fn with_preedit_support(mut self) -> Self {
        self.preedit_supported = true;
        self
    }

    /// Script the sequence of `acquire` outcomes (one popped per call; the last
    /// is reused once the queue drains).
    pub fn with_acquires(mut self, outcomes: impl IntoIterator<Item = AcquireOutcome>) -> Self {
        self.acquires = outcomes.into_iter().collect();
        self
    }

    /// Lose focus on **every** utterance's target, delivered through its focus
    /// stream when the controller polls it - where the daemon's own focus call
    /// reaches the controller (a single-consumer stream once hid a focus-loss
    /// safety bug for utterances 2+). Until the event is read the target still
    /// owns the field, so only the controller's reaction to it stops a write.
    pub fn with_focus_event(mut self, event: FocusEvent) -> Self {
        self.focus = Some(event);
        self
    }

    /// A handle that loses focus at a moment the test chooses.
    pub fn focus_sender(&self) -> FocusSender {
        FocusSender(self.lease.clone())
    }

    /// Lose focus while `acquire` runs, which then fails like `IbusInjector`'s.
    pub fn with_focus_event_during_acquire(mut self, event: FocusEvent) -> Self {
        self.focus_during_acquire = Some(event);
        self
    }

    /// Lose focus while a target's first `commit` is in flight, as a real
    /// backend does when focus moves during the round trip: that write is
    /// refused, and no focus event can have reached the controller ahead of
    /// the refusal.
    pub fn with_focus_event_during_commit(mut self, event: FocusEvent) -> Self {
        self.focus_during_commit = Some(event);
        self
    }

    /// A shared handle to the call log — clone before handing the mock away.
    pub fn log(&self) -> Arc<Mutex<InjectorLog>> {
        self.log.clone()
    }

    fn next_acquire(&mut self) -> AcquireOutcome {
        if self.acquires.len() > 1 {
            self.acquires.pop_front().unwrap()
        } else {
            self.acquires
                .front()
                .cloned()
                .unwrap_or(AcquireOutcome::NoTarget)
        }
    }
}

#[async_trait]
impl Injector for MockInjector {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        self.log.lock().unwrap().acquires += 1;
        let id = self.lease.borrow().id + 1;
        self.lease.send_replace(Lease { id, lost: None });
        if let Some(event) = self.focus_during_acquire {
            self.focus_sender().send(event);
        }
        match self.next_acquire() {
            AcquireOutcome::Ok if self.lease.borrow().loss(id).is_some() => {
                Err(InjectError::FocusLost)
            }
            AcquireOutcome::Ok => Ok(Box::new(MockTarget {
                id,
                lease: self.lease.subscribe(),
                lose_on_focus_poll: self.focus.map(|event| (self.focus_sender(), event)),
                lose_on_commit: self
                    .focus_during_commit
                    .map(|event| (self.focus_sender(), event)),
                log: self.log.clone(),
            })),
            AcquireOutcome::Secure => Err(InjectError::SecureField),
            AcquireOutcome::NoTarget => Err(InjectError::NoTarget),
            AcquireOutcome::Unavailable(msg) => Err(InjectError::Unavailable(msg)),
        }
    }

    fn supports_preedit(&self) -> bool {
        self.preedit_supported
    }
}

/// The [`Target`] a [`MockInjector`] hands out.
#[derive(Debug)]
pub struct MockTarget {
    id: u64,
    lease: watch::Receiver<Lease>,
    /// Focus lost as the controller reads it off this target's focus stream
    /// ([`MockInjector::with_focus_event`]).
    lose_on_focus_poll: Option<(FocusSender, FocusEvent)>,
    /// Focus lost during the first `commit`, if the test scripted one
    /// ([`MockInjector::with_focus_event_during_commit`]).
    lose_on_commit: Option<(FocusSender, FocusEvent)>,
    log: Arc<Mutex<InjectorLog>>,
}

impl MockTarget {
    fn owned(&self) -> bool {
        self.lease.borrow().loss(self.id).is_none()
    }
}

#[async_trait]
impl Target for MockTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        {
            let mut log = self.log.lock().unwrap();
            log.commits.push(text.to_string());
            log.order.push("commit");
        }
        // Mid-flight loss: the lease dies with the write already under way.
        if let Some((focus, event)) = self.lose_on_commit.take() {
            focus.send(event);
        }
        if self.owned() {
            Ok(())
        } else {
            Err(InjectError::FocusLost)
        }
    }

    async fn set_preedit(&mut self, text: &str) {
        let mut log = self.log.lock().unwrap();
        log.preedits.push(text.to_string());
        log.order.push("preedit");
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        let mut lease = self.lease.clone();
        let id = self.id;
        let scripted = self.lose_on_focus_poll.clone();
        stream::once(async move {
            // The loss lands as it is read, so a write attempted before the
            // controller read it would still have gone into the field.
            if let Some((focus, event)) = scripted {
                focus.send(event);
                return event;
            }
            match lease.wait_for(|l| l.loss(id).is_some()).await {
                Ok(l) => l.loss(id).unwrap_or(FocusEvent::FocusOut),
                Err(_) => FocusEvent::FocusOut,
            }
        })
        .boxed()
    }

    async fn release(self: Box<Self>) {
        self.log.lock().unwrap().releases += 1;
    }
}
