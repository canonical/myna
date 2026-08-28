//! The activity-indicator boundary (plan T22, UD129 Activity Indicator).
//!
//! A persistent, screen-reader-perceivable surface showing recording /
//! transcribing / finalizing / error — so the user always knows dictation is
//! live. [`notify::NotifyIndicator`] is the shipped default;
//! [`mock::MockIndicator`] is the hermetic test fixture. The former GTK
//! overlay (`indicator::gtk`, feature `ui-gtk`) was removed in T150 — the
//! myna-shell overlay (feature 004) and the headless notify path are the
//! shipped indicators. See `specs/003-desktop-injection/contracts/indicator.md`.

use async_trait::async_trait;

pub mod dbus;
pub mod mock;
pub mod notify;

/// The distinct, screen-reader-perceivable indicator states (FR-017/019). Never
/// carries transcript text (commit-only, privacy — N8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorState {
    /// No dictation in progress — the indicator is cleared.
    Hidden,
    /// Capturing / listening (also shown during a cold model load).
    Recording,
    /// Inference is decoding.
    Transcribing,
    /// Release seen; awaiting the terminal transcript.
    Finalizing,
    /// An error / secure-field refusal, with a user-facing message.
    ///
    /// `recoverable` (feature 004, 2026-07-30 HUD redesign, data-model E1a)
    /// distinguishes a non-blocking issue the user can immediately retry past
    /// (e.g. a session that completed with no speech captured) from a
    /// critical failure that persists until acknowledged (e.g. no microphone
    /// available). This is an interim, client-inferred classification ahead
    /// of a true wire-level error disposition (T31/T62) — see
    /// `controller::completion_indicator_state`. Non-D-Bus indicators
    /// (`gtk`/`notify`) currently render every `Error` identically regardless
    /// of this field (out of scope for feature 004); only `indicator::dbus`
    /// branches on it.
    Error { message: String, recoverable: bool },
}

impl IndicatorState {
    /// A critical, persistent error (`recoverable: false`) — the pre-2026-07-30
    /// behavior of `Error(msg)`, kept as a convenience constructor so call
    /// sites read naturally. Persists until the user acknowledges it (D-Bus:
    /// until dismissed; other indicators: until the session/state clears).
    pub fn critical(message: impl Into<String>) -> Self {
        IndicatorState::Error {
            message: message.into(),
            recoverable: false,
        }
    }

    /// A recoverable, non-blocking issue (`recoverable: true`) — e.g. a
    /// session that completed with nothing captured. Auto-dismisses on the
    /// D-Bus/HUD path (feature 004); non-D-Bus indicators render it exactly
    /// like a critical error today (out of scope for feature 004).
    pub fn recoverable(message: impl Into<String>) -> Self {
        IndicatorState::Error {
            message: message.into(),
            recoverable: true,
        }
    }
}

/// The activity-indicator seam. `set_state` is idempotent per state.
#[async_trait]
pub trait Indicator: Send {
    /// Show the given state (appears within the activation-latency target after
    /// `Recording` — SC-005).
    async fn set_state(&mut self, state: IndicatorState);

    /// Clear the indicator (equivalent to `set_state(Hidden)`).
    async fn hide(&mut self);
}
