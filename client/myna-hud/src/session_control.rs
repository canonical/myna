//! session_control — the `--serve-dbus` simulator's session-control state
//! machine (feature 004, T132; contract `dbus-interface.md` C6/C7), ported
//! from `dictation_service.py`'s `_on_method_call`.
//!
//! This is the pure half of the served `org.myna.Dictation` interface's
//! methods: `Start`, `Stop`, `Toggle`. It owns the one rule that has teeth —
//! **there is exactly one session, and duplicate `Start`/`Toggle` calls
//! never stack a second one** (C6, mirroring the controller's own
//! `ControlTrigger` dedup) — and leaves the bus plumbing to the wiring
//! (T132's zbus server).
//!
//! It is the simulator, not the real daemon, so `Start` always succeeds:
//! there is no microphone or backend to be unavailable. The `(ok, error)`
//! return shape is kept so the served method matches the contract and a
//! client sees the same signature it would from `myna-desktop` (C7).

/// The result of a `Start` (or a `Toggle` that started a session).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartOutcome {
    /// A new session began.
    Started,
    /// A session was already running; this call did nothing (C6 dedup).
    AlreadyActive,
}

impl StartOutcome {
    /// The `(ok, error)` pair the wire `Start` method returns. The simulator
    /// never fails, and a duplicate is still "ok" — the session the caller
    /// asked for is running, which is all `Start` promises.
    pub fn to_wire(self) -> (bool, &'static str) {
        (true, "")
    }
}

/// The simulator's single dictation session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Session {
    active: bool,
}

impl Session {
    /// Whether a session is currently running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Set the session flag directly.
    ///
    /// Used by the simulator when the lab's chosen state implies the session
    /// (a non-idle selection is a live session). The `Start`/`Stop`/`Toggle`
    /// methods remain the contract surface; this is the lab driving the same
    /// flag from the other side.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// `Start`: begin a session, or report that one is already running.
    ///
    /// Idempotent by contract (C6): a second `Start` does not begin a second
    /// session.
    pub fn start(&mut self) -> StartOutcome {
        if self.active {
            StartOutcome::AlreadyActive
        } else {
            self.active = true;
            StartOutcome::Started
        }
    }

    /// `Stop`: end the active session. A no-op when already idle.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// `Toggle`: `Start` if idle, else `Stop`.
    ///
    /// Returns the [`StartOutcome`] when it started a session, or `None`
    /// when it stopped one — so the caller can answer `Start`'s `(ok,
    /// error)` shape only in the case that maps to it.
    pub fn toggle(&mut self) -> Option<StartOutcome> {
        if self.active {
            self.stop();
            None
        } else {
            Some(self.start())
        }
    }
}
