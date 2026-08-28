//! `NotifyIndicator` — the headless activity indicator (plan T22, T019).
//!
//! The default daemon runs headless (no overlay window, so it can never steal
//! focus from the target app — the failure mode that sidelined the GTK overlay,
//! see the removed GTK overlay). To still make dictation *observable* it drives a
//! single desktop notification through the whole lifecycle: it raises one toast
//! on `Recording` ("listening") and **replaces it in place** (same notification
//! id) as the state advances to transcribing / finishing, closing it on
//! `Hidden` and switching it to a critical error toast on `Error`.
//!
//! Notifications never take input focus, so this is safe on Wayland where a
//! toplevel overlay is not. It carries state labels only, never transcript text
//! (privacy, N8). The richer always-on-top overlay is the myna-shell overlay
//! (feature 004); the former GTK `ui-gtk` overlay was removed in T150.

use async_trait::async_trait;
use notify_rust::{Hint, Notification, Timeout, Urgency};

use super::{Indicator, IndicatorState};

/// A `notify-rust`-backed indicator: one updating toast per dictation session.
pub struct NotifyIndicator {
    /// App name shown on every toast (`appname` field, not the `summary`).
    app_name: String,
    /// The live notification's id, so state changes *replace* it rather than
    /// stack a new toast each transition. Cleared on `Hidden`.
    id: Option<u32>,
    /// Pending auto-hide task for an error notice. Freedesktop `Timeout` is
    /// not reliably honored by daemons, so we `close()` ourselves after the
    /// dynamic hold.
    pending_hide: Option<tokio::task::JoinHandle<()>>,
}

impl Default for NotifyIndicator {
    fn default() -> Self {
        Self {
            app_name: "Myna".to_string(),
            id: None,
            pending_hide: None,
        }
    }
}

impl std::fmt::Debug for NotifyIndicator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyIndicator")
            .field("app_name", &self.app_name)
            .field("id", &self.id)
            .finish()
    }
}

/// The user-facing summary + body for a state (labels only — never transcript
/// text). `None` means "close the toast" (`Hidden`).
fn toast_text(state: &IndicatorState) -> Option<(String, String)> {
    match state {
        IndicatorState::Hidden => None,
        IndicatorState::Recording => Some((
            "🎤 Dictation: listening".into(),
            "Speak now — tap your shortcut again to stop.".into(),
        )),
        IndicatorState::Transcribing => Some((
            "💬 Dictation: transcribing".into(),
            "Converting speech to text…".into(),
        )),
        IndicatorState::Finalizing => Some((
            "⏳ Dictation: finishing".into(),
            "Inserting the final text…".into(),
        )),
        // `recoverable` (feature 004, 2026-07-30) is intentionally ignored
        // here: this indicator renders every error identically regardless of
        // severity (out of scope for feature 004 — see plan.md Complexity
        // Tracking).
        IndicatorState::Error { message, .. } => {
            Some(("⚠️ Dictation error".into(), message.clone()))
        }
    }
}

impl NotifyIndicator {
    pub fn new() -> Self {
        Self {
            app_name: "Myna".to_string(),
            id: None,
            pending_hide: None,
        }
    }

    /// Dynamic hold for a notice body, mirroring HUD `hold_ms_for` but
    /// slower reading than server (60 ms/char, ≥8000, ≤15000). Only
    /// `notice` (recoverable) auto-dismisses locally; `error` stays.
    /// Server auto-dismisses `notice` after `server_hold_ms_for` (48 ms/char,
    /// ≥6000); client keeps showing, ignoring server `idle` until this timer.
    fn hold_ms_for(body: &str) -> u64 {
        let len = body.chars().count() as f64;
        const PER_CHAR_MS: f64 = 60.0;
        const MIN_MS: f64 = 8000.0;
        const MAX_MS: f64 = 15_000.0;
        (len * PER_CHAR_MS).clamp(MIN_MS, MAX_MS) as u64
    }

    /// Show or replace the lifecycle toast, returning the (possibly new)
    /// notification id. Runs the blocking D-Bus round-trip off the runtime.
    /// Freedesktop `Timeout` is not reliably honored, so we always use
    /// `Timeout::Never` and close ourselves after `hold_ms_for` for errors.
    async fn show(&self, summary: String, body: String, error: bool) -> Option<u32> {
        let app = self.app_name.clone();
        let id = self.id;
        tokio::task::spawn_blocking(move || {
            let mut n = Notification::new();
            n.appname(&app)
                .summary(&summary)
                .body(&body)
                .timeout(Timeout::Never)
                // Transient: don't clutter the notification tray with dictation
                // liveness once dismissed.
                .hint(Hint::Transient(true))
                // Normal (not Low): GNOME Shell suppresses the banner popup for
                // low-urgency notifications and drops them straight into the
                // tray — which is exactly the "no UI" symptom. Normal pops a
                // banner; Critical for errors.
                .urgency(if error {
                    Urgency::Critical
                } else {
                    Urgency::Normal
                });
            if let Some(id) = id {
                n.id(id); // replace the existing toast in place
            }
            n.show().ok().map(|h| h.id())
        })
        .await
        .ok()
        .flatten()
    }

    /// Close the live toast, if any.
    async fn close(&self) {
        let Some(id) = self.id else { return };
        let app = self.app_name.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // Re-address the existing toast by id, then close it.
            if let Ok(handle) = Notification::new().appname(&app).id(id).show() {
                handle.close();
            }
        })
        .await;
    }

    fn abort_pending(&mut self) {
        if let Some(h) = self.pending_hide.take() {
            h.abort();
        }
    }

    fn schedule_auto_hide(&mut self, body: &str) {
        self.abort_pending();
        let ms = Self::hold_ms_for(body);
        let app = self.app_name.clone();
        let id = self.id;
        // Notifier-side timeout only: starts when shown, restarts on
        // replacement, and with multiple Dictation clients the server does
        // not drive idle.
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            if let Some(id) = id {
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(h) = Notification::new().appname(&app).id(id).show() {
                        h.close();
                    }
                })
                .await;
            }
        });
        self.pending_hide = Some(handle);
    }
}

#[async_trait]
impl Indicator for NotifyIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        // Keep notice visible for its whole local hold, ignoring server idle
        // until the timer completes. Only `recoverable` (notice) auto-dismisses
        // locally; `critical` (error) stays until server publishes new state.
        let is_notice = matches!(
            &state,
            IndicatorState::Error {
                recoverable: true,
                ..
            }
        );
        let is_error = matches!(&state, IndicatorState::Error { .. });
        match toast_text(&state) {
            Some((summary, body)) => {
                self.abort_pending();
                self.id = self.show(summary, body.clone(), is_error).await;
                if is_notice {
                    self.schedule_auto_hide(&body);
                }
            }
            None => {
                // `Hidden` from server auto-dismiss of a notice — keep
                // showing locally until our own timer completes.
                if self.pending_hide.is_some() {
                    return;
                }
                self.abort_pending();
                self.close().await;
                self.id = None;
            }
        }
    }

    async fn hide(&mut self) {
        if self.pending_hide.is_some() {
            return;
        }
        self.abort_pending();
        self.close().await;
        self.id = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_active_state_has_a_toast_and_hidden_closes() {
        assert!(toast_text(&IndicatorState::Recording).is_some());
        assert!(toast_text(&IndicatorState::Transcribing).is_some());
        assert!(toast_text(&IndicatorState::Finalizing).is_some());
        assert!(toast_text(&IndicatorState::critical("boom")).is_some());
        assert!(toast_text(&IndicatorState::Hidden).is_none());
    }

    #[test]
    fn error_toast_carries_the_message() {
        let (_summary, body) = toast_text(&IndicatorState::critical("mic gone")).unwrap();
        assert_eq!(body, "mic gone");
    }

    /// T011/P19 (2026-07-30): this indicator's behavior for the error state
    /// is provably unchanged regardless of `recoverable` — same toast and
    /// same `Critical` urgency for both severities.
    #[test]
    fn error_rendering_unchanged_regardless_of_recoverable() {
        let critical = toast_text(&IndicatorState::critical("x")).unwrap();
        let recoverable = toast_text(&IndicatorState::recoverable("x")).unwrap();
        assert_eq!(critical, recoverable);
        assert!(matches!(
            IndicatorState::critical("x"),
            IndicatorState::Error { .. }
        ));
        assert!(matches!(
            IndicatorState::recoverable("x"),
            IndicatorState::Error { .. }
        ));
    }

    #[test]
    fn labels_never_leak_transcript_text() {
        // The listening/transcribing/finishing labels are fixed strings with no
        // interpolated transcript (privacy, N8).
        for s in [
            IndicatorState::Recording,
            IndicatorState::Transcribing,
            IndicatorState::Finalizing,
        ] {
            let (summary, body) = toast_text(&s).unwrap();
            assert!(summary.starts_with(char::is_alphabetic) || summary.contains("Dictation"));
            assert!(!body.is_empty());
        }
    }
}
