//! `MockInjector` — the hermetic text-injection fixture (T007).
//!
//! Scripts `acquire` outcomes and a `focus_events` stream, and records every
//! `commit` / `set_preedit` / `set_activity` / `cancel` / `end` call so
//! controller tests can assert commit order/count, idempotent teardown, and the
//! commit-only invariant — with no IBus, D-Bus, or display. `supports_preedit()`
//! is `false` by default (commit-only; contract injector.md); preedit tests opt
//! in via [`MockInjector::with_preedit_support`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::{FocusEvent, InjectError, InjectionTarget, Injector};

/// A recording of what the controller did to the injector. Shared with the test
/// via [`MockInjector::log`] so assertions survive the controller owning the
/// injector.
#[derive(Debug, Default)]
pub struct InjectorLog {
    /// Committed segment texts, in order (commit-only invariant).
    pub commits: Vec<String>,
    /// Preedit texts passed to `set_preedit`, in order (volatile — must never
    /// also appear in `commits`).
    pub preedits: Vec<String>,
    /// Interleaved commit/preedit operation order (`"commit"` / `"preedit"`),
    /// so tests can assert a pending commit always lands *before* the preedit
    /// tail that follows it.
    pub order: Vec<&'static str>,
    /// `set_activity` toggles, in order.
    pub activity: Vec<bool>,
    /// Number of `acquire` calls.
    pub acquires: usize,
    /// Number of `cancel` calls (idempotency check).
    pub cancels: usize,
    /// Number of `end` calls (idempotency check).
    pub ends: usize,
    /// Number of times the prior IME/global-engine was "restored" (I11): once
    /// per teardown that actually released an acquired target.
    pub restores: usize,
}

/// The outcome a scripted `acquire()` yields.
#[derive(Debug, Clone)]
pub enum AcquireOutcome {
    /// Bind a normal editable target with the given opaque id.
    Ok(String),
    /// A password/secure field — `Err(SecureField)`.
    Secure,
    /// Nothing editable focused — `Err(NoTarget)`.
    NoTarget,
    /// Backend unreachable — `Err(Unavailable(msg))`.
    Unavailable(String),
}

/// A hermetic [`Injector`] driven by a script. Clone the [`InjectorLog`] handle
/// (`.log()`) *before* moving the mock into the controller to read it afterward.
pub struct MockInjector {
    acquires: VecDeque<AcquireOutcome>,
    focus: Vec<FocusEvent>,
    /// True once a target is acquired and not yet released (drives restore-once).
    acquired: bool,
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
    /// emits no focus events.
    pub fn new() -> Self {
        Self {
            acquires: VecDeque::from([AcquireOutcome::Ok("mock-target".into())]),
            focus: Vec::new(),
            acquired: false,
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

    /// Script focus events delivered on the `focus_events` stream (in order).
    /// The script is delivered on **every** `focus_events` call — each utterance
    /// gets its own focus stream, matching the real injector's broadcast (a
    /// single-consumer stream hid a focus-loss safety bug for utterances 2+).
    pub fn with_focus_events(mut self, events: impl IntoIterator<Item = FocusEvent>) -> Self {
        self.focus = events.into_iter().collect();
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
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError> {
        self.log.lock().unwrap().acquires += 1;
        match self.next_acquire() {
            AcquireOutcome::Ok(id) => {
                self.acquired = true;
                Ok(InjectionTarget::new(id, false))
            }
            AcquireOutcome::Secure => Err(InjectError::SecureField),
            AcquireOutcome::NoTarget => Err(InjectError::NoTarget),
            AcquireOutcome::Unavailable(msg) => Err(InjectError::Unavailable(msg)),
        }
    }

    async fn set_activity(&mut self, active: bool) {
        self.log.lock().unwrap().activity.push(active);
    }

    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        let mut log = self.log.lock().unwrap();
        log.commits.push(text.to_string());
        log.order.push("commit");
        Ok(())
    }

    async fn set_preedit(&mut self, text: &str) {
        let mut log = self.log.lock().unwrap();
        log.preedits.push(text.to_string());
        log.order.push("preedit");
    }

    fn supports_preedit(&self) -> bool {
        self.preedit_supported
    }

    async fn cancel(&mut self) {
        let mut log = self.log.lock().unwrap();
        log.cancels += 1;
        if self.acquired {
            log.restores += 1;
            self.acquired = false;
        }
    }

    async fn end(&mut self) {
        let mut log = self.log.lock().unwrap();
        log.ends += 1;
        if self.acquired {
            log.restores += 1;
            self.acquired = false;
        }
    }

    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
        stream::iter(self.focus.clone()).boxed()
    }
}
