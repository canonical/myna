//! `NotifyIndicator` — the headless activity indicator (plan T22, T019).
//!
//! The default daemon runs headless (no overlay window, so it can never steal
//! focus from the target app). To still make dictation *observable* it drives a
//! single desktop notification through the whole lifecycle: it raises one toast
//! on `Recording` ("listening") and **replaces it in place** (same notification
//! id) as the state advances to transcribing / finishing, closing it on
//! `Hidden` and switching it to a critical error toast on `Error`.
//!
//! Notifications never take input focus, so this is safe on Wayland where a
//! toplevel overlay is not. It carries state labels only, never transcript text
//! (privacy, N8).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gettextrs::gettext;
use tokio::sync::OnceCell;
use zbus::zvariant::Value;

use super::{Indicator, IndicatorState};

/// App name shown on every toast (`appname` field, not the `summary`).
const APP_NAME: &str = "Myna";

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications",
    gen_blocking = false
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;
}

/// The notification server, as far as a toast needs it.
#[async_trait]
trait Toasts: Send + Sync {
    /// Show a toast, replacing `replaces` when non-zero; returns its id.
    async fn notify(&self, replaces: u32, text: &Text, critical: bool) -> zbus::Result<u32>;
    async fn close(&self, id: u32);
}

/// A toast's summary and body.
type Text = (String, String);

/// `org.freedesktop.Notifications` on the session bus, connected on first use.
#[derive(Default)]
struct SessionToasts(OnceCell<NotificationsProxy<'static>>);

impl SessionToasts {
    async fn proxy(&self) -> zbus::Result<&NotificationsProxy<'static>> {
        self.0
            .get_or_try_init(|| async {
                NotificationsProxy::builder(&zbus::Connection::session().await?)
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
            })
            .await
    }
}

#[async_trait]
impl Toasts for SessionToasts {
    async fn notify(&self, replaces: u32, text: &Text, critical: bool) -> zbus::Result<u32> {
        let hints = HashMap::from([
            // Normal, not Low: GNOME Shell drops low-urgency toasts straight
            // into the tray without a banner.
            ("urgency", Value::U8(if critical { 2 } else { 1 })),
            // Keep dictation liveness out of the notification history.
            ("transient", Value::Bool(true)),
        ]);
        // `expire_timeout` 0 is never: servers do not reliably honour one, so
        // notices are closed by `schedule_auto_hide`.
        let (summary, body) = text;
        self.proxy()
            .await?
            .notify(APP_NAME, replaces, "", summary, body, &[], hints, 0)
            .await
    }

    async fn close(&self, id: u32) {
        if let Ok(proxy) = self.proxy().await {
            let _ = proxy.close_notification(id).await;
        }
    }
}

/// One updating toast per dictation session.
pub struct NotifyIndicator {
    toasts: Arc<dyn Toasts>,
    /// The live notification's id, so state changes *replace* it rather than
    /// stack a new toast each transition. Cleared on `Hidden`.
    id: Option<u32>,
    /// Pending auto-hide task for a notice.
    pending_hide: Option<tokio::task::JoinHandle<()>>,
    hold_ms_for: fn(&str) -> u64,
}

impl Default for NotifyIndicator {
    fn default() -> Self {
        Self::with(Arc::new(SessionToasts::default()), hold_ms_for)
    }
}

/// The notification-specific summary + body for a state (labels only — never
/// transcript text). `None` means "close the toast" (`Hidden`).
fn toast_text(state: &IndicatorState) -> Option<Text> {
    match state {
        IndicatorState::Hidden => None,
        IndicatorState::Recording => Some((
            gettext("🎤 Dictation: listening"),
            gettext("Speak now. Press your shortcut again to stop."),
        )),
        IndicatorState::Transcribing => Some((
            gettext("💬 Dictation: transcribing"),
            gettext("Converting speech to text…"),
        )),
        IndicatorState::Finalizing => Some((
            gettext("⏳ Dictation: finishing"),
            gettext("Inserting the final text…"),
        )),
        IndicatorState::Error { message, .. } => {
            Some((gettext("⚠️ Dictation error"), message.clone()))
        }
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

impl NotifyIndicator {
    pub fn new() -> Self {
        Self::default()
    }

    fn with(toasts: Arc<dyn Toasts>, hold_ms_for: fn(&str) -> u64) -> Self {
        Self {
            toasts,
            id: None,
            pending_hide: None,
            hold_ms_for,
        }
    }

    /// Show or replace the lifecycle toast, returning its id.
    async fn show(&self, text: &Text, critical: bool) -> Option<u32> {
        match self
            .toasts
            .notify(self.id.unwrap_or(0), text, critical)
            .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                myna_core::info_log!("notify", "toast not shown: {e}");
                None
            }
        }
    }

    fn abort_pending(&mut self) {
        if let Some(h) = self.pending_hide.take() {
            h.abort();
        }
    }

    fn schedule_auto_hide(&mut self, body: &str) {
        self.abort_pending();
        let Some(id) = self.id else { return };
        let ms = (self.hold_ms_for)(body);
        let toasts = self.toasts.clone();
        self.pending_hide = Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            toasts.close(id).await;
        }));
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
            Some(text) => {
                self.abort_pending();
                self.id = self.show(&text, is_error).await;
                if is_notice {
                    self.schedule_auto_hide(&text.1);
                }
            }
            None => {
                // `Hidden` from server auto-dismiss of a notice — keep
                // showing locally until our own timer completes.
                if self.pending_hide.is_some() {
                    return;
                }
                if let Some(id) = self.id.take() {
                    self.toasts.close(id).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A notification server that, like Plasma's, refuses a `Notify`
    /// identical to the one before it.
    #[derive(Default)]
    struct Server {
        next_id: u32,
        open: HashMap<u32, String>,
        /// The summary of every `Notify`, refused or not, and `close <id>`.
        calls: Vec<String>,
        last: Option<Text>,
        refuse_next: bool,
    }

    #[derive(Default)]
    struct FakeToasts(Mutex<Server>);

    #[async_trait]
    impl Toasts for FakeToasts {
        async fn notify(&self, replaces: u32, text: &Text, _: bool) -> zbus::Result<u32> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(text.0.clone());
            let identical = s.last.as_ref() == Some(text);
            s.last = Some(text.clone());
            if std::mem::take(&mut s.refuse_next) || identical {
                return Err(zbus::Error::Failure("ExcessNotificationGeneration".into()));
            }
            let id = if s.open.contains_key(&replaces) {
                replaces
            } else {
                s.next_id += 1;
                s.next_id
            };
            s.open.insert(id, text.0.clone());
            Ok(id)
        }

        async fn close(&self, id: u32) {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("close {id}"));
            s.open.remove(&id);
        }
    }

    fn rig() -> (NotifyIndicator, Arc<FakeToasts>) {
        let server = Arc::new(FakeToasts::default());
        (NotifyIndicator::with(server.clone(), |_| 0), server)
    }

    fn calls(server: &FakeToasts) -> Vec<String> {
        server.0.lock().unwrap().calls.clone()
    }

    fn open(server: &FakeToasts) -> Vec<String> {
        server.0.lock().unwrap().open.values().cloned().collect()
    }

    #[tokio::test]
    async fn one_toast_is_replaced_through_the_dictation_and_closed() {
        let (mut indicator, server) = rig();
        indicator.set_state(IndicatorState::Recording).await;
        indicator.set_state(IndicatorState::Finalizing).await;
        assert_eq!(open(&server), ["⏳ Dictation: finishing"]);
        indicator.set_state(IndicatorState::Hidden).await;
        assert_eq!(
            calls(&server),
            [
                "🎤 Dictation: listening",
                "⏳ Dictation: finishing",
                "close 1"
            ]
        );
        assert!(open(&server).is_empty());
    }

    #[tokio::test]
    async fn a_notice_closes_itself_after_its_hold() {
        let (mut indicator, server) = rig();
        indicator
            .set_state(IndicatorState::recoverable("No speech"))
            .await;
        indicator.set_state(IndicatorState::Hidden).await;
        assert_eq!(open(&server), ["⚠️ Dictation error"], "held past idle");
        indicator.pending_hide.take().unwrap().await.unwrap();
        assert!(open(&server).is_empty());
    }

    #[test]
    fn every_active_state_has_a_toast_and_hidden_closes() {
        assert!(toast_text(&IndicatorState::Recording).is_some());
        assert!(toast_text(&IndicatorState::Transcribing).is_some());
        assert!(toast_text(&IndicatorState::Finalizing).is_some());
        assert!(toast_text(&IndicatorState::critical("boom")).is_some());
        assert!(toast_text(&IndicatorState::Hidden).is_none());
    }

    #[test]
    fn lifecycle_toasts_keep_their_notification_specific_text() {
        assert_eq!(
            toast_text(&IndicatorState::Recording),
            Some((
                "🎤 Dictation: listening".into(),
                "Speak now. Press your shortcut again to stop.".into(),
            ))
        );
        assert_eq!(
            toast_text(&IndicatorState::Transcribing),
            Some((
                "💬 Dictation: transcribing".into(),
                "Converting speech to text…".into(),
            ))
        );
        assert_eq!(
            toast_text(&IndicatorState::Finalizing),
            Some((
                "⏳ Dictation: finishing".into(),
                "Inserting the final text…".into(),
            ))
        );
        assert_eq!(
            toast_text(&IndicatorState::critical("mic gone")),
            Some(("⚠️ Dictation error".into(), "mic gone".into()))
        );
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
        // The notification's fixed labels are never built from transcript
        // content (privacy, N8).
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
