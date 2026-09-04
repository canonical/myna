//! `DbusTrigger` — a [`Trigger`] backend fed by the `com.canonical.Myna.Dictation`
//! `Start`/`Stop`/`Toggle` D-Bus methods (feature 004, contract publisher.md
//! P9–P12), sibling to `ControlTrigger` with the same alternation/dedup so the
//! panel button is equivalent to the hotkey (C6).
//!
//! The served `com.canonical.Myna.Dictation` object holds a [`DbusTriggerSource`] and
//! calls [`Start`](DbusTriggerSource::start)/[`Stop`](DbusTriggerSource::stop)
//! /[`Toggle`](DbusTriggerSource::toggle) in its method handlers; each call
//! maps to a [`TriggerEdge`] with the same Press/Release alternation and
//! duplicate-suppression `ControlTrigger` uses (P9/P10). The trigger's
//! [`Trigger::next_edge`] yields those edges to the controller.
//!
//! `Start` also answers the `(ok, reason)` shape (C7/P11): the reason is
//! content-free, and a refused start pushes no edge — the trigger never
//! panics. The startability gate itself is the controller's concern; the
//! source exposes [`DbusTriggerSource::refuse`] for the refused path.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use myna_orchestrator::{Trigger, TriggerEdge};

/// The edges a session can be in, for the alternation/dedup bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    Idle,
    Active,
}

/// The half the served object holds: pushes edges into the trigger.
///
/// The methods mirror the wire `Start`/`Stop`/`Toggle`. Each yields one edge
/// and only ever when the session-state actually flips, so duplicate calls
/// never start a second session (P10, mirroring `ControlTrigger`).
#[derive(Clone)]
pub struct DbusTriggerSource {
    tx: mpsc::Sender<TriggerEdge>,
    state: Arc<Mutex<SessionState>>,
}

impl DbusTriggerSource {
    fn edge(&self, edge: TriggerEdge) {
        // Best-effort: if the trigger (and thus the channel) is gone, the
        // method call is simply a no-op — never block dictation on it.
        let _ = self.tx.try_send(edge);
    }

    fn flip(&self, target: SessionState, edge: TriggerEdge) {
        let mut state = self.state.lock().expect("trigger state poisoned");
        if *state == target {
            return; // already there — duplicate call, no edge (P10)
        }
        *state = target;
        drop(state);
        self.edge(edge);
    }

    /// `Start`: begin a session. Yields `Press` when idle; a duplicate while
    /// active pushes nothing (P10).
    pub fn start(&self) {
        self.flip(SessionState::Active, TriggerEdge::Press);
    }

    /// `Stop`: end the session. Yields `Release` when active; a no-op when
    /// already idle.
    pub fn stop(&self) {
        self.flip(SessionState::Idle, TriggerEdge::Release);
    }

    /// `Toggle`: `Start` if idle, else `Stop` (C6).
    pub fn toggle(&self) {
        let mut state = self.state.lock().expect("trigger state poisoned");
        if *state == SessionState::Idle {
            *state = SessionState::Active;
            drop(state);
            self.edge(TriggerEdge::Press);
        } else {
            *state = SessionState::Idle;
            drop(state);
            self.edge(TriggerEdge::Release);
        }
    }

    /// A refused start: pushes no edge (P11) — the served method reports the
    /// `(false, reason)` and the session never begins.
    pub fn refuse(&self) {}
}

/// The [`Trigger`] half: yields the edges the source pushed.
pub struct DbusTrigger {
    rx: mpsc::Receiver<TriggerEdge>,
    _source: DbusTriggerSource,
}

impl DbusTrigger {
    /// Build a trigger + its source. The source is what the served object
    /// holds and calls; the trigger is handed to the controller.
    pub fn new() -> (Self, DbusTriggerSource) {
        let (tx, rx) = mpsc::channel(8);
        let source = DbusTriggerSource {
            tx,
            state: Arc::new(Mutex::new(SessionState::Idle)),
        };
        (
            Self {
                rx,
                _source: source.clone(),
            },
            source,
        )
    }
}

#[async_trait]
impl Trigger for DbusTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        self.rx.recv().await
    }

    /// Drop queued edges without flipping parity, like `ControlTrigger`.
    async fn discard_pending(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }
}
