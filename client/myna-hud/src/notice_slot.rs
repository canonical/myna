//! notice_slot — the held-notice slot (feature 004; research R15;
//! FR-007a/FR-007b/FR-007d; contract extension.md X20, re-homed to the
//! renderer by the 2026-08-26 architecture revision). Ported from the rules
//! `hud.js` implemented inside its actor.
//!
//! There is exactly **one** slot, never a queue: any new problem descriptor
//! replaces whatever is held (see [`crate::hud_logic::should_replace_held_notice`]).
//! The two tiers differ only in how they clear:
//!
//! * **recoverable** — auto-dismisses after [`HOLD_MS`]; a replacement
//!   restarts that window *in full*, so a second "no speech detected" right
//!   after the first does not clear on the original's stale schedule.
//! * **critical** — never auto-clears; it persists until the user's explicit
//!   dismiss, and a replacement does not waive that requirement.
//!
//! The slot is pure and clock-free: the caller passes "now" (monotonic ms)
//! and owns the actual timer/redraw. That keeps the rules testable without a
//! toolkit and stops a stray timer from ever outliving the window.

use crate::hud_logic::severity_auto_dismisses;
use crate::states::Severity;

/// How long a recoverable notice is held before it clears itself. The same
/// bounded window the indicator has always used (spec Assumptions: reuse the
/// existing ~3.5 s hold rather than introducing a new tunable).
pub const HOLD_MS: f64 = 3500.0;

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
    /// A problem severity replaces whatever is held (restarting the hold for
    /// a recoverable one); `None` — any live, non-problem state — clears the
    /// slot, so a new session always starts clean.
    pub fn hold(&mut self, severity: Option<Severity>, reason: &str, now_ms: f64) {
        match severity {
            None => self.dismiss(),
            Some(severity) => {
                self.severity = Some(severity);
                self.reason = reason.to_string();
                self.expires_at = if severity_auto_dismisses(Some(severity)) {
                    Some(now_ms + HOLD_MS)
                } else {
                    None
                };
            }
        }
    }

    /// The user's explicit dismiss (the × control), or a live state
    /// superseding the notice.
    pub fn dismiss(&mut self) {
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
