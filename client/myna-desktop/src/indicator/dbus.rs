//! `DbusIndicator` — an [`Indicator`] backend that publishes dictation state
//! onto `com.canonical.Myna.Dictation` for the GNOME Shell extension (feature 004,
//! contract publisher.md P1–P5). Composes with `NotifyIndicator` as the
//! fallback when the session bus is unavailable (P15).

use std::sync::Arc;

use async_trait::async_trait;

use crate::dbus::{PropertyValue, SharedBus};
use crate::indicator::readiness::Readiness;
use crate::indicator::{status_message, Indicator, IndicatorState};

/// The `com.canonical.Myna.Dictation` wire states (data-model E1; the additive string
/// enum the extension switches on).
pub mod wire_state {
    /// No session; HUD pill hidden.
    pub const IDLE: &str = "idle";
    /// Cold model load in progress (the `Loading`-seen / `Ready`-not-yet
    /// window, R4/C5).
    pub const LOADING: &str = "loading";
    /// Capturing / listening (post-`Ready`).
    pub const RECORDING: &str = "recording";
    /// Inference decoding.
    pub const TRANSCRIBING: &str = "transcribing";
    /// Release seen; awaiting the terminal transcript.
    pub const FINALIZING: &str = "finalizing";
    /// A recoverable, non-blocking issue (2026-07-30, feature 004 HUD
    /// redesign, data-model E1/E1a/R13) — e.g. a session that completed with
    /// no speech captured. Additive: an unpatched extension build degrades
    /// this to the neutral "active" treatment (FR-008), never a crash or a
    /// stuck error.
    pub const NOTICE: &str = "notice";
    /// A critical failure / secure-field refusal.
    pub const ERROR: &str = "error";
}

/// Map an [`IndicatorState`] to the `com.canonical.Myna.Dictation` `State` string
/// (data-model E1 table). `ready_seen` splits `Recording` into the cold-load
/// `loading` window vs post-`Ready` `recording` (R4/C5). `Error{recoverable}`
/// splits into `notice` (recoverable) vs `error` (critical) — the two are
/// mutually exclusive per call (data-model E1a, R13, contract C10). Pure and
/// total — the output is always one of the seven wire states, so no
/// transcript text can ever cross into the payload through this path (C3).
pub fn map_state(state: &IndicatorState, ready_seen: bool) -> &'static str {
    use wire_state::*;
    match state {
        IndicatorState::Hidden => IDLE,
        IndicatorState::Recording if ready_seen => RECORDING,
        IndicatorState::Recording => LOADING,
        IndicatorState::Transcribing => TRANSCRIBING,
        IndicatorState::Finalizing => FINALIZING,
        IndicatorState::Error {
            recoverable: true, ..
        } => NOTICE,
        IndicatorState::Error {
            recoverable: false, ..
        } => ERROR,
    }
}

/// Server-side auto-dismiss for `notice` (recoverable) and `error` (critical):
/// after a longer hold the server publishes `idle` so late-joining clients do
/// not keep seeing stale status forever.
/// Hold is dynamic per text length, longer than the notifier's previous
/// hold but shorter than the client's new slower hold.
fn server_hold_ms_for(reason: &str) -> u64 {
    let len = reason.chars().count() as f64;
    const PER_CHAR_MS: f64 = 48.0;
    const MIN_MS: f64 = 6000.0;
    const MAX_MS: f64 = 12000.0;
    (len * PER_CHAR_MS).clamp(MIN_MS, MAX_MS) as u64
}

/// Publishes `IndicatorState` transitions as `State`/`StatusMessage` property
/// updates via the [`crate::dbus::Bus`] seam (P1) — pushed to subscribers with
/// the standard `PropertiesChanged`, the interface's only signal (contract
/// §Confinement). Emits exactly one `State` update per *wire-state* transition
/// (C2 — a duplicate `IndicatorState` whose mapped state is unchanged is a
/// no-op) and carries only state + a content-free reason (C3).
pub struct DbusIndicator {
    bus: SharedBus,
    readiness: Readiness,
    /// Last published `(state, status_message)` — drives the one-signal-per-
    /// transition dedup and the publisher-owned status label invariant.
    /// Shared with the pending auto-dismiss task so it can check whether the
    /// current transient state is still current before publishing `idle`.
    last: Arc<tokio::sync::Mutex<Option<(String, String)>>>,
    /// Pending server-side auto-dismiss for the current transient state.
    pending_hide: Option<tokio::task::JoinHandle<()>>,
    /// Auto-dismiss hold for a status message, keyed by its length. A plain
    /// fn pointer so tests can inject a near-instant hold.
    hold_ms_for: fn(&str) -> u64,
}

impl DbusIndicator {
    /// Publish over `bus`, splitting `recording`/`loading` per `readiness`.
    pub fn new(bus: SharedBus, readiness: Readiness) -> Self {
        Self {
            bus,
            readiness,
            last: Arc::new(tokio::sync::Mutex::new(None)),
            pending_hide: None,
            hold_ms_for: server_hold_ms_for,
        }
    }

    /// Test constructor: override the auto-dismiss hold so a test does not
    /// wait on the real 6–12 s timer.
    #[cfg(test)]
    fn with_hold(bus: SharedBus, readiness: Readiness, hold_ms_for: fn(&str) -> u64) -> Self {
        Self {
            bus,
            readiness,
            last: Arc::new(tokio::sync::Mutex::new(None)),
            pending_hide: None,
            hold_ms_for,
        }
    }

    fn cancel_auto_hide(&mut self) {
        if let Some(h) = self.pending_hide.take() {
            h.abort();
        }
    }

    fn schedule_auto_hide(&mut self, status_message: String) {
        self.cancel_auto_hide();
        let ms = (self.hold_ms_for)(&status_message);
        let bus = self.bus.clone();
        let last = self.last.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            // Only auto-dismiss if still the same transient state; otherwise
            // a new state has overwritten `last` and this task should have
            // been aborted. Check `last` before publishing.
            {
                let guard = last.lock().await;
                let is_still_this = matches!(
                    &*guard,
                    Some((s, r))
                        if (s == wire_state::NOTICE || s == wire_state::ERROR)
                            && r == &status_message
                );
                if !is_still_this {
                    return;
                }
            }
            // Publish idle, clearing the levels and status (P3).
            {
                let mut bus = bus.lock().await;
                bus.set_property("StatusMessage", PropertyValue::Str(String::new()))
                    .await;
                bus.set_property("State", PropertyValue::Str(wire_state::IDLE.to_string()))
                    .await;
                bus.set_property("AudioRms", PropertyValue::F64(0.0)).await;
                bus.set_property("AudioPeak", PropertyValue::F64(0.0)).await;
            }
            // Update `last` to idle so future `publish` dedup is correct.
            {
                let mut guard = last.lock().await;
                *guard = Some((wire_state::IDLE.to_string(), String::new()));
            }
        });
        self.pending_hide = Some(handle);
    }

    /// Publish the transition (unless it repeats the current wire state) as
    /// `StatusMessage` + `State` property sets, each pushed to subscribers
    /// via `PropertiesChanged` (C2). The message goes first so a client
    /// reacting to the `State` flip already reads the consistent label.
    async fn publish(&mut self, state: &str, status_message: &str) {
        let current = (state.to_string(), status_message.to_string());
        {
            let guard = self.last.lock().await;
            if guard.as_ref() == Some(&current) {
                return; // idempotent per wire state (Indicator seam contract)
            }
        }
        {
            let mut guard = self.last.lock().await;
            *guard = Some(current);
        }

        let mut bus = self.bus.lock().await;
        bus.set_property(
            "StatusMessage",
            PropertyValue::Str(status_message.to_string()),
        )
        .await;
        bus.set_property("State", PropertyValue::Str(state.to_string()))
            .await;
    }
}

#[async_trait]
impl Indicator for DbusIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        let wire = map_state(&state, self.readiness.ready_seen());
        let status_message = status_message(&state, self.readiness.ready_seen());
        let is_transient = wire == wire_state::NOTICE || wire == wire_state::ERROR;
        // Cancel any pending hide on any state change; a new transient state
        // will re-arm with its own reason.
        self.cancel_auto_hide();
        self.publish(wire, &status_message).await;
        if is_transient {
            self.schedule_auto_hide(status_message);
        }
    }

    async fn set_audio_drops(&mut self, not_active: u64) {
        let mut bus = self.bus.lock().await;
        bus.set_property("AudioDroppedNotActive", PropertyValue::U64(not_active))
            .await;
    }

    async fn set_input_levels(&mut self, levels: super::InputLevels) {
        let mut bus = self.bus.lock().await;
        for (name, value) in [
            ("InputOffset", levels.offset),
            ("InputNoiseFloor", levels.noise_floor),
            ("InputSpeechLevel", levels.speech_level),
            ("InputSnr", levels.snr_db.unwrap_or(f32::NAN)),
        ] {
            bus.set_property(name, PropertyValue::F64(value.into()))
                .await;
        }
    }

    /// Outside [`Self::publish`]'s dedup and untouched by the auto-hide: a
    /// second failure with the same headline still updates the detail and
    /// time, and Diagnostics opened after the pill is gone still reads it.
    async fn set_last_error(&mut self, headline: &str, detail: &str) {
        let mut bus = self.bus.lock().await;
        crate::dbus::publish_last_error(&mut *bus, headline, detail).await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::dbus::FakeBus;

    #[tokio::test]
    async fn drop_counts_are_published_as_counts_and_nothing_else() {
        let fake = FakeBus::new();
        let service = crate::dbus::DictationService::new(fake.clone());
        let mut indicator = DbusIndicator::new(service.bus(), Readiness::default());

        indicator.set_audio_drops(3).await;

        assert_eq!(
            fake.property("AudioDroppedNotActive"),
            Some(PropertyValue::U64(3))
        );
        assert!(fake.property("State").is_none());
    }

    #[tokio::test]
    async fn input_levels_are_published_as_doubles() {
        let fake = FakeBus::new();
        let service = crate::dbus::DictationService::new(fake.clone());
        let mut indicator = DbusIndicator::new(service.bus(), Readiness::default());

        indicator
            .set_input_levels(super::super::InputLevels {
                offset: -0.25,
                noise_floor: 0.001,
                speech_level: 0.0,
                snr_db: Some(12.5),
            })
            .await;

        assert_eq!(
            fake.property("InputOffset"),
            Some(PropertyValue::F64(-0.25))
        );
        assert_eq!(
            fake.property("InputNoiseFloor"),
            Some(PropertyValue::F64(f64::from(0.001f32)))
        );
        assert_eq!(
            fake.property("InputSpeechLevel"),
            Some(PropertyValue::F64(0.0))
        );
        assert_eq!(fake.property("InputSnr"), Some(PropertyValue::F64(12.5)));
        assert!(fake.property("State").is_none());

        indicator
            .set_input_levels(super::super::InputLevels {
                snr_db: None,
                ..Default::default()
            })
            .await;
        assert!(
            matches!(fake.property("InputSnr"), Some(PropertyValue::F64(v)) if v.is_nan()),
            "no estimate is NaN"
        );
    }

    /// Contract publisher.md P1 / data-model E1: every IndicatorState maps to
    /// the com.canonical.Myna.Dictation State string of the E1 table.
    #[test]
    fn indicator_state_maps_to_wire_state() {
        assert_eq!(map_state(&IndicatorState::Hidden, false), "idle");
        assert_eq!(
            map_state(&IndicatorState::Transcribing, true),
            "transcribing"
        );
        assert_eq!(map_state(&IndicatorState::Finalizing, true), "finalizing");
        assert_eq!(
            map_state(&IndicatorState::critical("refusing to type"), true),
            "error"
        );
    }

    /// R4/C5: the cold-load window — `Recording` seen before `Ready` —
    /// publishes `loading`, and only post-`Ready` `recording`.
    #[test]
    fn recording_splits_on_readiness() {
        assert_eq!(map_state(&IndicatorState::Recording, false), "loading");
        assert_eq!(map_state(&IndicatorState::Recording, true), "recording");
    }

    #[test]
    fn publisher_owns_status_messages_for_every_visible_state() {
        let cases = [
            (IndicatorState::Recording, false, "Loading model…"),
            (IndicatorState::Recording, true, "Listening"),
            (IndicatorState::Transcribing, true, "Transcribing"),
            (IndicatorState::Finalizing, true, "Finishing"),
            (
                IndicatorState::recoverable("No speech detected"),
                true,
                "No speech detected",
            ),
            (
                IndicatorState::critical("Microphone unavailable"),
                true,
                "Error: Microphone unavailable",
            ),
        ];
        for (state, ready, expected) in cases {
            let wire = map_state(&state, ready);
            assert_eq!(status_message(&state, ready), expected, "{wire}");
        }
        assert_eq!(status_message(&IndicatorState::Hidden, false), "");
    }

    /// Critical errors are published as `Error: <message>` (so the severity
    /// reads in the HUD text, not just the tint), keeping the message verbatim
    /// after the prefix; recoverable notices publish the reason verbatim.
    #[test]
    fn critical_errors_are_prefixed_notices_verbatim() {
        let critical = status_message(&IndicatorState::critical("microphone unavailable"), true);
        assert_eq!(critical, "Error: microphone unavailable");
        let critical = status_message(&IndicatorState::critical("no backend is connected"), true);
        assert_eq!(critical, "Error: no backend is connected");
        let notice = status_message(&IndicatorState::recoverable("No speech detected"), true);
        assert_eq!(notice, "No speech detected", "notices are not prefixed");
    }

    /// C3: the mapping can only emit one of the seven contract state strings —
    /// no payload it produces can carry transcript text.
    #[test]
    fn mapping_outputs_are_content_free() {
        const WIRE_STATES: [&str; 7] = [
            "idle",
            "loading",
            "recording",
            "transcribing",
            "finalizing",
            "notice",
            "error",
        ];
        for ready in [false, true] {
            for state in [
                IndicatorState::Hidden,
                IndicatorState::Recording,
                IndicatorState::Transcribing,
                IndicatorState::Finalizing,
                IndicatorState::critical("a transcript would go here"),
                IndicatorState::recoverable("a transcript would go here"),
            ] {
                assert!(WIRE_STATES.contains(&map_state(&state, ready)));
            }
        }
    }

    /// T009/C10 (2026-07-30, R13): `Error{recoverable: true}` maps to the new
    /// `notice` wire state; `recoverable: false` still maps to `error`. The
    /// two are mutually exclusive per call.
    #[test]
    fn error_severity_splits_notice_from_error() {
        assert_eq!(
            map_state(&IndicatorState::recoverable("no speech detected"), true),
            "notice"
        );
        assert_eq!(
            map_state(&IndicatorState::critical("microphone unavailable"), true),
            "error"
        );
        // Readiness must not affect the severity split either way.
        assert_eq!(
            map_state(&IndicatorState::recoverable("no speech detected"), false),
            "notice"
        );
    }

    /// P16: `notice` and `error` are mutually exclusive per `IndicatorState`
    /// value — no input maps to both.
    #[test]
    fn notice_and_error_are_mutually_exclusive() {
        let notice = map_state(&IndicatorState::recoverable("x"), true);
        let error = map_state(&IndicatorState::critical("x"), true);
        assert_ne!(notice, error);
        assert_eq!(notice, "notice");
        assert_eq!(error, "error");
    }

    /// A critical error auto-dismisses to `idle` after the hold, clearing the
    /// status message, so it cannot linger forever on the wire.
    #[tokio::test]
    async fn critical_error_auto_hides_after_hold() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut indicator = DbusIndicator::with_hold(shared, Readiness::new(), |_| 5);

        indicator
            .set_state(IndicatorState::critical("mic gone"))
            .await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str("error".into())),
            "critical error publishes `error` immediately"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str(wire_state::IDLE.into())),
            "critical error auto-hides to `idle` after the hold"
        );
        assert_eq!(
            bus.property("StatusMessage"),
            Some(PropertyValue::Str(String::new())),
            "auto-hide clears the status message"
        );
    }

    /// A recoverable notice keeps auto-hiding to `idle` after the hold.
    #[tokio::test]
    async fn recoverable_notice_auto_hides_after_hold() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut indicator = DbusIndicator::with_hold(shared, Readiness::new(), |_| 5);

        indicator
            .set_state(IndicatorState::recoverable("No speech detected"))
            .await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str("notice".into()))
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str(wire_state::IDLE.into())),
            "notice auto-hides to `idle` after the hold"
        );
    }

    /// A state change before the hold fires cancels the pending hide: a stale
    /// timer must not yank a newer state back to `idle`.
    #[tokio::test]
    async fn state_change_before_hold_cancels_pending_hide() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut indicator = DbusIndicator::with_hold(shared, Readiness::new(), |_| 100);

        indicator
            .set_state(IndicatorState::critical("mic gone"))
            .await;
        indicator.set_state(IndicatorState::Transcribing).await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str(wire_state::TRANSCRIBING.into()))
        );

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str(wire_state::TRANSCRIBING.into())),
            "a stale auto-hide must not fire after the state moved on"
        );
    }

    fn last_error_time(bus: &FakeBus) -> i64 {
        match bus.property("LastErrorTime") {
            Some(PropertyValue::I64(t)) => t,
            other => panic!("LastErrorTime: {other:?}"),
        }
    }

    /// The last error outlives the pill: Diagnostics is opened long after the
    /// auto-hide, and a later success does not erase it either.
    #[tokio::test]
    async fn the_last_error_survives_the_auto_hide_and_later_success() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut indicator = DbusIndicator::with_hold(shared, Readiness::new(), |_| 5);

        indicator
            .set_last_error(
                "Model not running",
                "myna-whisper is connected but not running",
            )
            .await;
        indicator
            .set_state(IndicatorState::critical("Model not running"))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        indicator.set_state(IndicatorState::Recording).await;
        indicator.set_state(IndicatorState::Hidden).await;

        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str(wire_state::IDLE.into()))
        );
        assert_eq!(
            bus.property("LastError"),
            Some(PropertyValue::Str("Model not running".into()))
        );
        assert_eq!(
            bus.property("LastErrorDetail"),
            Some(PropertyValue::Str(
                "myna-whisper is connected but not running".into()
            ))
        );
        assert!(last_error_time(&bus) > 0);
    }

    /// The pill dedups on what it shows; the last error must not, or a second
    /// failure that reads the same would keep the first one's cause.
    #[tokio::test]
    async fn the_same_headline_twice_updates_the_detail_and_time() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut indicator = DbusIndicator::with_hold(shared, Readiness::new(), |_| 100);

        indicator.set_last_error("Model error", "first").await;
        indicator
            .set_state(IndicatorState::critical("Model error"))
            .await;
        let first = last_error_time(&bus);
        std::thread::sleep(Duration::from_millis(2));
        indicator.set_last_error("Model error", "second").await;
        indicator
            .set_state(IndicatorState::critical("Model error"))
            .await;

        assert_eq!(
            bus.property("LastErrorDetail"),
            Some(PropertyValue::Str("second".into()))
        );
        assert!(last_error_time(&bus) > first);
        assert_eq!(
            bus.state_history().iter().filter(|s| *s == "error").count(),
            1,
            "the pill itself still dedups"
        );
    }
}
