//! `NotifyIndicator` — the headless / error-toast activity indicator (plan T22,
//! T019).
//!
//! The MVP indicator: it has no persistent surface (that is the GTK overlay,
//! branch 003d/US3), so it raises a desktop notification on error/refusal states
//! and stays quiet otherwise — enough for the controller to run and report
//! failures without GTK. Notifications carry state labels only, never transcript
//! text (privacy, N8).

use async_trait::async_trait;
use notify_rust::Notification;

use super::{Indicator, IndicatorState};

/// A `notify-rust`-backed indicator (error toasts / headless fallback).
#[derive(Debug, Default)]
pub struct NotifyIndicator {
    /// The active notification's app-name summary.
    app_name: String,
}

impl NotifyIndicator {
    pub fn new() -> Self {
        Self { app_name: "myna dictation".to_string() }
    }

    /// Fire a desktop notification off the async runtime (the `show()` D-Bus
    /// round-trip is blocking).
    async fn notify(&self, summary: String, body: String) {
        let app = self.app_name.clone();
        let _ = tokio::task::spawn_blocking(move || {
            Notification::new().summary(&app).body(&format!("{summary}: {body}")).show()
        })
        .await;
    }
}

#[async_trait]
impl Indicator for NotifyIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        // Only errors/refusals warrant a headless toast; the transient
        // recording/transcribing/finalizing states have no persistent surface
        // here (that is the GTK overlay). Never include transcript text.
        if let IndicatorState::Error(message) = state {
            self.notify("dictation error".to_string(), message).await;
        }
    }

    async fn hide(&mut self) {}
}
