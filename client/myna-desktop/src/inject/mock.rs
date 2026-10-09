//! `MockInjector` — the hermetic text-injection fixture (T007).
//!
//! Scripts `acquire` outcomes and focus loss, models the field it writes into
//! (text, preedit, whether it is secure), and records every `commit` /
//! `set_preedit` / `release` so controller tests can assert commit order and
//! count, teardown, and the commit-only invariant - with no IBus, D-Bus, or
//! display. Its targets hold a lease like `IbusInjector`'s: focus loss is
//! retained, and output after it is refused (recorded as refused, so a test
//! sees the controller attempt it). A loss scripted with
//! [`MockInjector::with_focus_event`] is delivered through the target's focus
//! stream, so a controller that fails to act on the event still holds a live
//! target and writes into it. Its capabilities are commit-only with secure
//! field detection by default; preedit tests opt in via
//! [`MockInjector::with_preedit_support`].

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::watch;

use super::{
    Activation, FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

/// A recording of what the controller did to the injector. Shared with the test
/// via [`MockInjector::log`] so assertions survive the controller owning the
/// injector.
#[derive(Debug, Default)]
pub struct InjectorLog {
    /// Commits the target accepted, in order: what reached the field.
    pub commits: Vec<String>,
    /// Commits the target refused (lease gone, secure field), in order.
    pub refused: Vec<String>,
    /// Every `set_preedit` call, in order, whether or not the field showed it
    /// (volatile, never also in `commits`).
    pub preedits: Vec<String>,
    /// Interleaved commit/preedit call order (`"commit"` / `"preedit"`),
    /// so tests can assert a pending commit always lands *before* the preedit
    /// tail that follows it.
    pub order: Vec<&'static str>,
    /// Number of `acquire` calls.
    pub acquires: usize,
    /// Number of targets released, each restoring the prior engine once (I11).
    pub releases: usize,
    /// Number of `activated` calls on held targets.
    pub activations: usize,
    /// Number of signals through [`Injector::activation`].
    pub acquire_activations: usize,
}

impl InjectorLog {
    /// Commits attempted, accepted or refused.
    pub fn attempts(&self) -> usize {
        self.commits.len() + self.refused.len()
    }
}

/// The outcome a scripted `acquire()` yields. A secure field is the field's
/// state ([`MockField::set_secure`]), not a scripted outcome.
#[derive(Debug, Clone)]
pub enum AcquireOutcome {
    /// Bind the field.
    Ok,
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
    /// Its target was told of an activation.
    activated: bool,
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

/// What the field shows and what it is.
#[derive(Debug, Default)]
struct Shown {
    text: String,
    preedit: String,
    secure: bool,
}

/// The test's hand on the field the mock writes into. Every acquire binds it
/// afresh, as if the user had refocused it.
#[derive(Clone, Debug)]
pub struct MockField {
    lease: Arc<watch::Sender<Lease>>,
    shown: Arc<Mutex<Shown>>,
}

impl MockField {
    /// Focus leaves the field, ending the lease of the target handed out
    /// last; the toolkit discards the preedit with it.
    pub fn lose_focus(&self, event: FocusEvent) {
        self.lease.send_modify(|lease| {
            lease.lost.get_or_insert(event);
        });
        self.shown.lock().unwrap().preedit.clear();
    }

    /// Focus leaves the field and comes straight back, as an X11 key grab
    /// makes it: the toolkit discards the preedit, and the lease rides it out
    /// only if its target was told of an activation.
    pub fn blip(&self) {
        if self.lease.borrow().activated {
            self.shown.lock().unwrap().preedit.clear();
        } else {
            self.lose_focus(FocusEvent::FocusOut);
        }
    }

    /// The field's content type turns secure (or ordinary) under the user.
    pub fn set_secure(&self, secure: bool) {
        self.shown.lock().unwrap().secure = secure;
    }

    /// The committed text the field holds.
    pub fn text(&self) -> String {
        self.shown.lock().unwrap().text.clone()
    }

    /// The preedit the field shows.
    pub fn preedit(&self) -> String {
        self.shown.lock().unwrap().preedit.clone()
    }
}

/// A hermetic [`Injector`] driven by a script. Clone the [`InjectorLog`] handle
/// (`.log()`) *before* moving the mock into the controller to read it afterward.
pub struct MockInjector {
    acquire: AcquireOutcome,
    /// Focus lost as each target's focus stream is polled.
    focus: Option<FocusEvent>,
    /// Focus lost while `acquire` runs.
    focus_during_acquire: Option<FocusEvent>,
    /// Focus lost while a target's first `commit` is in flight.
    focus_during_commit: Option<FocusEvent>,
    field: MockField,
    capabilities: TextInputCapabilities,
    log: Arc<Mutex<InjectorLog>>,
    /// How long `acquire` takes.
    acquire_delay: std::time::Duration,
}

impl Default for MockInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl MockInjector {
    /// A mock whose every `acquire` binds its field, which never loses focus.
    pub fn new() -> Self {
        Self {
            acquire: AcquireOutcome::Ok,
            focus: None,
            focus_during_acquire: None,
            focus_during_commit: None,
            field: MockField {
                lease: Arc::new(watch::Sender::new(Lease {
                    id: 0,
                    lost: Some(FocusEvent::FocusOut),
                    activated: false,
                })),
                shown: Arc::default(),
            },
            capabilities: TextInputCapabilities {
                preedit: false,
                surrounding_text: false,
                secure_field_detection: Support::Supported,
            },
            log: Arc::new(Mutex::new(InjectorLog::default())),
            acquire_delay: std::time::Duration::ZERO,
        }
    }

    /// `acquire` takes `delay`, as one waiting for a key grab to end does.
    pub fn with_acquire_delay(mut self, delay: std::time::Duration) -> Self {
        self.acquire_delay = delay;
        self
    }

    /// Report a replacement-safe preedit region and show what `set_preedit`
    /// draws (the IBus backend's behavior; default is commit-only).
    pub fn with_preedit_support(mut self) -> Self {
        self.capabilities.preedit = true;
        self
    }

    /// Whether secure fields are recognised. Where `Unknown`, a secure field
    /// is written into like any other.
    pub fn with_secure_field_detection(mut self, detection: Support) -> Self {
        self.capabilities.secure_field_detection = detection;
        self
    }

    /// Script what every `acquire` yields.
    pub fn with_acquire(mut self, outcome: AcquireOutcome) -> Self {
        self.acquire = outcome;
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

    /// The field this mock writes into.
    pub fn field(&self) -> MockField {
        self.field.clone()
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

    fn detects_secure(&self) -> bool {
        self.capabilities.secure_field_detection == Support::Supported
    }
}

#[async_trait]
impl Injector for MockInjector {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        self.log.lock().unwrap().acquires += 1;
        if !self.acquire_delay.is_zero() {
            tokio::time::sleep(self.acquire_delay).await;
        }
        let lease = &self.field.lease;
        let id = lease.borrow().id + 1;
        lease.send_replace(Lease {
            id,
            lost: None,
            activated: false,
        });
        if let Some(event) = self.focus_during_acquire {
            self.field.lose_focus(event);
        }
        match self.acquire.clone() {
            AcquireOutcome::Ok if self.field.lease.borrow().loss(id).is_some() => {
                Err(InjectError::FocusLost)
            }
            AcquireOutcome::Ok
                if self.detects_secure() && self.field.shown.lock().unwrap().secure =>
            {
                Err(InjectError::SecureField)
            }
            AcquireOutcome::Ok => Ok(Box::new(MockTarget {
                id,
                lease: self.field.lease.subscribe(),
                field: self.field.clone(),
                capabilities: self.capabilities,
                lose_on_focus_poll: self.focus.map(|event| (self.field(), event)),
                lose_on_commit: self.focus_during_commit.map(|event| (self.field(), event)),
                log: self.log.clone(),
            })),
            AcquireOutcome::NoTarget => Err(InjectError::NoTarget),
            AcquireOutcome::Unavailable(msg) => Err(InjectError::Unavailable(msg)),
        }
    }

    fn capabilities(&self) -> TextInputCapabilities {
        self.capabilities
    }

    fn activation(&self) -> Activation {
        let log = self.log.clone();
        Activation::new(move || log.lock().unwrap().acquire_activations += 1)
    }
}

/// The [`Target`] a [`MockInjector`] hands out.
#[derive(Debug)]
pub struct MockTarget {
    id: u64,
    lease: watch::Receiver<Lease>,
    field: MockField,
    capabilities: TextInputCapabilities,
    /// Focus lost as the controller reads it off this target's focus stream
    /// ([`MockInjector::with_focus_event`]).
    lose_on_focus_poll: Option<(MockField, FocusEvent)>,
    /// Focus lost during the first `commit`, if the test scripted one
    /// ([`MockInjector::with_focus_event_during_commit`]).
    lose_on_commit: Option<(MockField, FocusEvent)>,
    log: Arc<Mutex<InjectorLog>>,
}

impl MockTarget {
    fn owned(&self) -> bool {
        self.lease.borrow().loss(self.id).is_none()
    }

    /// Why a write must not land now, if it must not.
    fn refusal(&self) -> Option<InjectError> {
        if !self.owned() {
            Some(InjectError::FocusLost)
        } else if self.capabilities.secure_field_detection == Support::Supported
            && self.field.shown.lock().unwrap().secure
        {
            Some(InjectError::SecureField)
        } else {
            None
        }
    }
}

#[async_trait]
impl Target for MockTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        if self.owned() {
            self.field.shown.lock().unwrap().preedit.clear();
        }
        // Mid-flight loss: the lease dies with the write already under way.
        if let Some((field, event)) = self.lose_on_commit.take() {
            field.lose_focus(event);
        }
        let refusal = self.refusal();
        let mut log = self.log.lock().unwrap();
        log.order.push("commit");
        match refusal {
            Some(err) => {
                log.refused.push(text.to_string());
                Err(err)
            }
            None => {
                log.commits.push(text.to_string());
                self.field.shown.lock().unwrap().text.push_str(text);
                Ok(())
            }
        }
    }

    async fn set_preedit(&mut self, text: &str) {
        {
            let mut log = self.log.lock().unwrap();
            log.preedits.push(text.to_string());
            log.order.push("preedit");
        }
        if self.capabilities.preedit && self.refusal().is_none() {
            self.field.shown.lock().unwrap().preedit = text.to_string();
        }
    }

    fn activated(&self) {
        self.log.lock().unwrap().activations += 1;
        let id = self.id;
        self.field
            .lease
            .send_modify(|lease| lease.activated |= lease.id == id);
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        let mut lease = self.lease.clone();
        let id = self.id;
        let scripted = self.lose_on_focus_poll.clone();
        stream::once(async move {
            // The loss lands as it is read, so a write attempted before the
            // controller read it would still have gone into the field.
            if let Some((field, event)) = scripted {
                field.lose_focus(event);
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
        if self.owned() {
            self.field.shown.lock().unwrap().preedit.clear();
            self.field.lease.send_modify(|lease| {
                lease.lost.get_or_insert(FocusEvent::FocusOut);
            });
        }
    }
}
