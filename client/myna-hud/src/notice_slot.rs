//! notice_slot — the held-notice slot (feature 004; research R15;
//! FR-007a/FR-007b/FR-007d; contract extension.md X20, re-homed to the
//! renderer by the 2026-08-26 architecture revision).
//!
//! There is exactly **one** slot, never a queue: any new problem descriptor
//! replaces whatever is held (see [`crate::hud_logic::should_replace_held_notice`]).
//! Only `Recoverable` (`notice`) auto-dismisses after its dynamic hold;
//! `Critical` (`error`) stays until the server publishes a new state.
//! The server auto-dismisses `notice` after `server_hold_ms_for` (longer);
//! the client keeps showing for `hold_ms_for` (even longer, slower reading)
//! and ignores the server's `idle` until its timer completes.

use crate::states::Severity;

/// How long a recoverable notice is held before it clears itself. Kept for
/// tests/compat — the actual hold is now dynamic (see `hold_ms_for`).
pub const HOLD_MS: f64 = 3500.0;

/// Minimum display time for a `notice` on the client. The server
/// auto-dismisses `notice` after `server_hold_ms_for` (longer), and the
/// client keeps showing for this (even longer, slower reading) minimum,
/// ignoring the server's `idle` until it expires. `error` never
/// auto-dismisses on either side.
pub fn hold_ms_for(reason: &str) -> f64 {
    let len = reason.chars().count() as f64;
    const PER_CHAR_MS: f64 = 60.0;
    const MIN_MS: f64 = 8000.0;
    const MAX_MS: f64 = 15_000.0;
    (len * PER_CHAR_MS).clamp(MIN_MS, MAX_MS)
}

/// The single held-notice slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoticeSlot {
    severity: Option<Severity>,
    reason: String,
    /// When the notice clears itself (`None` = never — a critical error).
    expires_at: Option<f64>,
}

impl NoticeSlot {
    /// Apply a descriptor's severity/reason at time `now_ms`.
    ///
    /// A problem severity replaces whatever is held, restarting the hold
    /// window in full. Only `Recoverable` (`notice`) auto-dismisses after
    /// `hold_ms_for(reason)` (client-side, slower reading); `Critical`
    /// (`error`) stays persistent until the server publishes a new state.
    /// The server will auto-dismiss `notice` after `server_hold_ms_for`
    /// (longer), but the client keeps showing for its own (even longer)
    /// hold, ignoring the server's `idle` until its timer completes. `None`
    /// (a live, non-problem state) clears the slot so a new session
    /// always starts clean. A replacement restarts the window in full.
    pub fn hold(&mut self, severity: Option<Severity>, reason: &str, now_ms: f64) {
        match severity {
            None => self.clear(),
            Some(sev) => {
                self.severity = Some(sev);
                self.reason = reason.to_string();
                self.expires_at = match sev {
                    Severity::Recoverable => Some(now_ms + hold_ms_for(reason)),
                    Severity::Critical => None,
                };
            }
        }
    }

    /// Clear the slot: a live state has superseded the notice.
    ///
    /// There is no user-facing dismiss control — the HUD takes no pointer
    /// input at all — so this is driven entirely by the client publishing a
    /// new state.
    pub fn clear(&mut self) {
        self.severity = None;
        self.reason.clear();
        self.expires_at = None;
    }

    /// Whether the notice is on screen at `now_ms`.
    pub fn is_showing(&self, now_ms: f64) -> bool {
        match (self.severity, self.expires_at) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(_), Some(expiry)) => now_ms <= expiry,
        }
    }

    /// The held severity, if any.
    pub fn severity(&self) -> Option<Severity> {
        self.severity
    }

    /// The held content-free reason, if any.
    pub fn reason(&self) -> Option<&str> {
        if self.severity.is_some() {
            Some(&self.reason)
        } else {
            None
        }
    }

    /// When the notice clears itself — `None` for a critical error (the
    /// caller schedules a redraw/teardown at this instant).
    pub fn expires_at(&self) -> Option<f64> {
        self.expires_at
    }
}
