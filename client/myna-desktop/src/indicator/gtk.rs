//! `GtkIndicator` — the persistent GTK4 activity-overlay indicator (plan T22,
//! T029, branch 003d/US3).
//!
//! A borderless, non-focusable GTK4 overlay window with distinct visuals per
//! [`IndicatorState`], AT-SPI-labelled for a11y (FR-019). The indicator lives on
//! the GTK main thread and the tokio controller pushes states to it over an
//! `async-channel`; the error state also raises a `notify-rust` toast (FR-020).
//!
//! Gated behind the opt-in `ui-gtk` feature; packaged builds omit it, so
//! neither the shipped snap nor the hermetic suite links GTK.
//! Visual/latency/AT-SPI behavior is proven by the env-gated display suite
//! (`tests/indicator_hw.rs`, T028) on hardware; the state timeline itself is
//! covered hermetically via `MockIndicator` (T027).
//!
//! ## Threading (R6)
//!
//! GTK owns the process main thread + GLib loop; the controller (tokio) runs on
//! a worker thread. [`GtkIndicator`] is just the `async-channel` sender the
//! controller holds; [`run_indicator_app`] builds the window on the main thread
//! and drains the channel via `glib::spawn_future_local`. The binary wires the
//! two (T030).

use async_trait::async_trait;

use super::{Indicator, IndicatorState};

/// The tokio-side handle: sends [`IndicatorState`]s to the GTK main-thread loop.
#[derive(Debug, Clone)]
pub struct GtkIndicator {
    tx: async_channel::Sender<IndicatorState>,
}

impl GtkIndicator {
    /// Create the indicator from the sender half of the bridge channel. Pair
    /// with [`run_indicator_app`] on the GTK main thread.
    pub fn new(tx: async_channel::Sender<IndicatorState>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Indicator for GtkIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        // A closed channel just means the GTK side is gone (shutting down).
        let _ = self.tx.send(state).await;
    }

    async fn hide(&mut self) {
        let _ = self.tx.send(IndicatorState::Hidden).await;
    }
}

/// Human-readable, screen-reader-perceivable label for a state (AT-SPI, FR-019).
/// Carries no transcript text (privacy, N8).
fn state_label(state: &IndicatorState) -> Option<&'static str> {
    match state {
        IndicatorState::Hidden => None,
        IndicatorState::Recording => Some("Dictation: listening"),
        IndicatorState::Transcribing => Some("Dictation: transcribing"),
        IndicatorState::Finalizing => Some("Dictation: finishing"),
        IndicatorState::Error { .. } => Some("Dictation: error"),
    }
}

/// A CSS class per state so the overlay is visually distinct.
fn state_css_class(state: &IndicatorState) -> &'static str {
    match state {
        IndicatorState::Hidden => "hidden",
        IndicatorState::Recording => "recording",
        IndicatorState::Transcribing => "transcribing",
        IndicatorState::Finalizing => "finalizing",
        IndicatorState::Error { .. } => "error",
    }
}

/// Run the GTK application that owns the overlay window, draining `rx` for state
/// updates. **Must be called on the process main thread** (GTK requirement);
/// blocks in the GLib main loop until the app quits. The tokio controller runs
/// on a worker thread and pushes states through the [`GtkIndicator`] paired with
/// `rx` (see the binary, T030).
pub fn run_indicator_app(rx: async_channel::Receiver<IndicatorState>) -> glib::ExitCode {
    use gtk::prelude::*;
    use gtk4 as gtk;

    let app = gtk::Application::builder()
        .application_id("com.canonical.Myna.Indicator")
        .build();

    app.connect_activate(move |app| {
        let css = gtk::CssProvider::new();
        css.load_from_data(
            ".recording { background: #b00020; color: white; }\
             .transcribing { background: #1a73e8; color: white; }\
             .finalizing { background: #188038; color: white; }\
             .error { background: #5f6368; color: white; }\
             label { padding: 6px 12px; font-weight: bold; }",
        );
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let label = gtk::Label::new(Some("Dictation"));
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("myna dictation")
            .decorated(false)
            .resizable(false)
            .default_width(180)
            .default_height(32)
            .child(&label)
            .build();
        // Non-focusable: the overlay must never steal input from the target.
        window.set_can_focus(false);
        window.set_focusable(false);

        let rx = rx.clone();
        let window_ref = window.clone();
        let app_ref = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(state) = rx.recv().await {
                match &state {
                    IndicatorState::Hidden => {
                        window_ref.set_visible(false);
                    }
                    other => {
                        // Reset visual classes, apply this state's.
                        for c in ["recording", "transcribing", "finalizing", "error"] {
                            label.remove_css_class(c);
                        }
                        label.add_css_class(state_css_class(other));
                        if let Some(text) = state_label(other) {
                            label.set_text(text);
                            // AT-SPI: expose the state to assistive tech (FR-019).
                            label.update_property(&[gtk::accessible::Property::Label(text)]);
                        }
                        window_ref.set_visible(true);

                        // The error state also raises a desktop notification (FR-020).
                        // `recoverable` (feature 004, 2026-07-30) is intentionally
                        // ignored here: this indicator renders every error
                        // identically regardless of severity (out of scope for
                        // feature 004 — see plan.md Complexity Tracking).
                        if let IndicatorState::Error { message, .. } = &state {
                            let msg = message.clone();
                            let _ = notify_rust::Notification::new()
                                .summary("myna dictation")
                                .body(&format!("dictation error: {msg}"))
                                .show();
                        }
                    }
                }
            }
            // The controller side dropped its sender (session loop ended) —
            // tear down the overlay app so the main thread returns.
            app_ref.quit();
        });
    });

    // Run without forwarding argv (the binary owns arg parsing).
    let args: [&str; 0] = [];
    app.run_with_args(&args)
}
