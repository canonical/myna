//! The activity-indicator boundary (plan T22, UD129 Activity Indicator).
//!
//! A persistent, screen-reader-perceivable surface showing recording /
//! transcribing / finalizing / error — so the user always knows dictation is
//! live. [`gtk::GtkIndicator`] (feature `ui-gtk`, branch 003d) is the shipped
//! overlay; [`notify::NotifyIndicator`] is the headless/error fallback;
//! [`mock::MockIndicator`] is the hermetic test fixture. See
//! `specs/003-desktop-injection/contracts/indicator.md`.

use async_trait::async_trait;

#[cfg(feature = "ui-gtk")]
pub mod gtk;
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
    Error(String),
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
