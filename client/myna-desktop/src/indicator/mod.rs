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
use gettextrs::gettext;
use myna_core::failure::FailurePresentation;

pub mod dbus;
pub mod dynamic;
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
    ///
    /// `presentation` (US4, T068, FR-024) is `Some` when `message` was built
    /// from a registered `FailurePresentation` (`crate::failure::lookup`/
    /// `lookup_by_code`) — carrying the `&'static` presentation through lets
    /// the announcer (`accessibility::format::format_state_announcement`)
    /// recover its fixed, `'static` `message`/`recovery_action` text for the
    /// AT-SPI announcement, which requires a compile-time-`'static` string
    /// (`AnnouncementText`) rather than the dynamic `message: String` here.
    /// `None` for the handful of ad-hoc recoverable notices this feature's
    /// contract deliberately leaves untouched ("No speech detected"/"Focus
    /// lost" — not part of `contracts/failure-mapping.md`'s scope).
    Error {
        message: String,
        recoverable: bool,
        presentation: Option<&'static FailurePresentation>,
    },
}

impl IndicatorState {
    /// A critical, persistent error (`recoverable: false`) — the pre-2026-07-30
    /// behavior of `Error(msg)`, kept as a convenience constructor so call
    /// sites read naturally. Persists until the user acknowledges it (D-Bus:
    /// until dismissed; other indicators: until the session/state clears).
    ///
    /// `presentation: None` — for ad-hoc messages outside `contracts/
    /// failure-mapping.md`'s scope (US4). Registry-backed failures use
    /// [`Self::from_failure`] instead.
    pub fn critical(message: impl Into<String>) -> Self {
        IndicatorState::Error {
            message: message.into(),
            recoverable: false,
            presentation: None,
        }
    }

    /// A recoverable, non-blocking issue (`recoverable: true`) — e.g. a
    /// session that completed with nothing captured. Auto-dismisses on the
    /// D-Bus/HUD path (feature 004); non-D-Bus indicators render it exactly
    /// like a critical error today (out of scope for feature 004).
    ///
    /// `presentation: None` — see [`Self::critical`]'s doc comment.
    pub fn recoverable(message: impl Into<String>) -> Self {
        IndicatorState::Error {
            message: message.into(),
            recoverable: true,
            presentation: None,
        }
    }

    /// Build an `Error` state from a registered [`FailurePresentation`] (US4,
    /// T068, FR-024): every surface that renders this state can recover the
    /// exact same fixed wording, so indicator/notification/terminal/
    /// announcement structurally cannot diverge (F2/F3). `detail`, if given,
    /// is dynamic, non-`'static` context (e.g. an `InjectError::Unavailable`
    /// backend's own message) appended after the fixed text — never as a
    /// replacement for it, so the primary text stays plain-language even
    /// when the detail itself is technical.
    pub fn from_failure(presentation: &'static FailurePresentation, detail: Option<&str>) -> Self {
        IndicatorState::Error {
            message: presentation.render(detail),
            recoverable: matches!(
                presentation.severity,
                myna_core::failure::Severity::Recoverable
            ),
            presentation: Some(presentation),
        }
    }
}

/// The publisher-owned, content-free label for a D-Bus `StatusMessage`.
///
/// This belongs to the indicator boundary rather than `indicator::dbus`:
/// D-Bus is one presentation transport, while the desktop session policy
/// decides whether `Recording` is still loading or actively listening.
/// Consumers render this already-translated value verbatim.
///
/// Critical errors are published as `Error: <message>` so the severity reads
/// in the HUD text itself, not just from the pill's tint; the message is kept
/// verbatim (lowercase) after the prefix. Recoverable notices publish the
/// reason verbatim too — they carry their own severity colour.
pub fn status_message(state: &IndicatorState, ready_seen: bool) -> String {
    match state {
        IndicatorState::Hidden => String::new(),
        IndicatorState::Recording if !ready_seen => gettext("Loading model…"),
        IndicatorState::Recording => gettext("Listening"),
        IndicatorState::Transcribing => gettext("Transcribing"),
        IndicatorState::Finalizing => gettext("Finishing"),
        IndicatorState::Error {
            message,
            recoverable: false,
            ..
        } => gettext("Error: %s").replace("%s", message),
        IndicatorState::Error {
            message,
            recoverable: true,
            ..
        } => message.clone(),
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

    /// Publish this session's running drop counts. Counts only - no audio, no
    /// content - and the one capture-health fact no reader outside the daemon
    /// can obtain for itself.
    async fn set_audio_drops(&mut self, _not_resident: u64, _not_active: u64) {}
}
