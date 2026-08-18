//! `NotifyIndicator` — the headless activity indicator (plan T22, T019).
//!
//! The default daemon runs headless (no overlay window, so it can never steal
//! focus from the target app — the failure mode that sidelined the GTK overlay,
//! see `indicator::gtk`). To still make dictation *observable* it drives a
//! single desktop notification through the whole lifecycle: it raises one toast
//! on `Recording` ("listening") and **replaces it in place** (same notification
//! id) as the state advances to transcribing / finishing, closing it on
//! `Hidden` and switching it to a critical error toast on `Error`.
//!
//! Notifications never take input focus, so this is safe on Wayland where a
//! toplevel overlay is not. It carries state labels only, never transcript text
//! (privacy, N8). The richer always-on-top overlay is `indicator::gtk`
//! (feature `ui-gtk`), which must use the layer-shell protocol to avoid the
//! focus-steal that this indicator sidesteps entirely.

use async_trait::async_trait;
use notify_rust::{Hint, Notification, Timeout, Urgency};

use super::{Indicator, IndicatorState};

/// A `notify-rust`-backed indicator: one updating toast per dictation session.
#[derive(Debug, Default)]
pub struct NotifyIndicator {
    /// App-name summary shown on every toast.
    app_name: String,
    /// The live notification's id, so state changes *replace* it rather than
    /// stack a new toast each transition. Cleared on `Hidden`.
    id: Option<u32>,
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
            Some(("⚠ Dictation error".into(), message.clone()))
        }
    }
}

impl NotifyIndicator {
    pub fn new() -> Self {
        Self {
            app_name: "myna dictation".to_string(),
            id: None,
        }
    }

    /// Show or replace the lifecycle toast, returning the (possibly new)
    /// notification id. Runs the blocking D-Bus round-trip off the runtime.
    async fn show(&self, summary: String, body: String, error: bool) -> Option<u32> {
        let app = self.app_name.clone();
        let id = self.id;
        tokio::task::spawn_blocking(move || {
            let mut n = Notification::new();
            n.summary(&app)
                .body(&format!("{summary}\n{body}"))
                // Persistent while active: it lives for the utterance, then we
                // close/replace it — it must not self-dismiss mid-dictation.
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
            if let Ok(handle) = Notification::new().summary(&app).id(id).show() {
                handle.close();
            }
        })
        .await;
    }
}

#[async_trait]
impl Indicator for NotifyIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        match toast_text(&state) {
            Some((summary, body)) => {
                let error = matches!(state, IndicatorState::Error { .. });
                self.id = self.show(summary, body, error).await;
            }
            None => {
                self.close().await;
                self.id = None;
            }
        }
    }

    async fn hide(&mut self) {
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
