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

use crate::states::Severity;

/// How long a recoverable notice is held before it clears itself. Kept for
/// tests/compat — the actual hold is now dynamic (see `hold_ms_for`).
pub const HOLD_MS: f64 = 3500.0;

/// Compute a dynamic hold duration from the visible text length, as done in
/// GNOME Shell's gdm `util.js` (`_getIntervalForMessage`): give the user
/// `48 ms` per character or `HOLD_MS` (3.5 s), whichever is longer. The
/// timeout starts when the notice is shown and restarts in full on a
/// replacement, so a second error right after the first is shown again for
/// its full interval. Capped at 10 s to avoid a very long error pinning
/// the HUD forever.
pub fn hold_ms_for(reason: &str) -> f64 {
    // `chars().count()` is the user-visible length; byte length would
    // over-count multi-byte emoji (e.g. "⚠️" is 6 bytes but 1 char).
    let len = reason.chars().count() as f64;
    const PER_CHAR_MS: f64 = 48.0;
    const MAX_MS: f64 = 10_000.0;
    (len * PER_CHAR_MS).max(HOLD_MS).min(MAX_MS)
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
    /// window in full. The hold window is dynamic — `hold_ms_for(reason)` —
    /// and applies to **both** severities; even a critical error now
    /// auto-dismisses after its interval and returns to idle. The timeout
    /// starts when the notice is shown and a replacement restarts it, so a
    /// new error while one is visible is shown again for its full interval.
    /// `None` (a live, non-problem state) clears the slot so a new session
    /// always starts clean. With multiple Dictation clients the server does
    /// not drive the timeout — each notifier (HUD, notification) owns its
    /// own timer locally — so a `State` that stays `error` on the bus still
    /// cycles: show → timeout → idle → new `error` publish re-shows.
    pub fn hold(&mut self, severity: Option<Severity>, reason: &str, now_ms: f64) {
        match severity {
            None => self.clear(),
            Some(severity) => {
                self.severity = Some(severity);
                self.reason = reason.to_string();
                self.expires_at = Some(now_ms + hold_ms_for(reason));
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
