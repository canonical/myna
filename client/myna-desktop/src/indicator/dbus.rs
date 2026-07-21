//! `DbusIndicator` — an [`Indicator`] backend that publishes dictation state
//! onto `org.myna.Dictation` for the GNOME Shell extension (feature 004,
//! contract publisher.md P1–P5). Composes with `NotifyIndicator` as the
//! fallback when the session bus is unavailable (P15).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use myna_orchestrator::{OrchestratorEvent, TextSink};

use crate::dbus::{PropertyValue, SharedBus};
use crate::indicator::{Indicator, IndicatorState};

/// The `org.myna.Dictation` wire states (data-model E1; the additive string
/// enum the extension switches on).
pub mod wire_state {
    /// No session; goop hidden.
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
    /// Failure / secure-field refusal.
    pub const ERROR: &str = "error";
}

/// Map an [`IndicatorState`] to the `org.myna.Dictation` `State` string
/// (data-model E1 table). `ready_seen` splits `Recording` into the cold-load
/// `loading` window vs post-`Ready` `recording` (R4/C5). Pure and total — the
/// output is always one of the six wire states, so no transcript text can ever
/// cross into the payload through this path (C3).
pub fn map_state(state: &IndicatorState, ready_seen: bool) -> &'static str {
    use wire_state::*;
    match state {
        IndicatorState::Hidden => IDLE,
        IndicatorState::Recording if ready_seen => RECORDING,
        IndicatorState::Recording => LOADING,
        IndicatorState::Transcribing => TRANSCRIBING,
        IndicatorState::Finalizing => FINALIZING,
        IndicatorState::Error(_) => ERROR,
    }
}

/// Session-scoped readiness tracking for the `loading`/`recording` split
/// (R4/P2): the controller maps both `Loading` and `Ready` orchestrator events
/// to `IndicatorState::Recording`, so the publisher keeps whether `Ready` has
/// been seen this session. A cheaply clonable handle — the [`ReadinessTee`]
/// writes it from the session's event stream while the [`DbusIndicator`] reads
/// it from the controller's indicator calls; the event is always observed
/// *before* the controller routes it to `set_state`, so the flag is fresh.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    ready_seen: Arc<AtomicBool>,
}

impl Readiness {
    /// A fresh, cold session (no `Ready` seen).
    pub fn new() -> Self {
        Self::default()
    }

    /// `Loading` seen — the session is in the cold-load window.
    pub fn note_loading(&self) {
        self.ready_seen.store(false, Ordering::SeqCst);
    }

    /// `Ready` seen — subsequent `Recording` publishes `recording`.
    pub fn note_ready(&self) {
        self.ready_seen.store(true, Ordering::SeqCst);
    }

    /// New session: back to cold until the next `Ready`.
    pub fn reset(&self) {
        self.ready_seen.store(false, Ordering::SeqCst);
    }

    /// Whether `Ready` has been seen this session.
    pub fn ready_seen(&self) -> bool {
        self.ready_seen.load(Ordering::SeqCst)
    }
}

/// A [`TextSink`] wrapper that tracks `Loading`/`Ready` for the publisher's
/// `loading`/`recording` split (R4), forwarding every event to the real sink
/// unchanged. Wired per session by the `--dbus` session factory; invisible to
/// the controller and to non-D-Bus modes.
pub struct ReadinessTee<S: TextSink> {
    inner: S,
    readiness: Readiness,
}

impl<S: TextSink> ReadinessTee<S> {
    /// Wrap `inner`, updating `readiness` as liveness events flow past.
    pub fn new(inner: S, readiness: Readiness) -> Self {
        Self { inner, readiness }
    }
}

#[async_trait]
impl<S: TextSink> TextSink for ReadinessTee<S> {
    async fn emit(&mut self, event: OrchestratorEvent) {
        match event {
            OrchestratorEvent::Loading => self.readiness.note_loading(),
            OrchestratorEvent::Ready => self.readiness.note_ready(),
            _ => {}
        }
        self.inner.emit(event).await;
    }
}

/// Publishes `IndicatorState` transitions as `StateChanged` signals + `State`
/// property updates via the [`crate::dbus::Bus`] seam (P1). Emits exactly one
/// signal per *wire-state* transition (C2 — a duplicate `IndicatorState` whose
/// mapped state is unchanged is a no-op), carries only state + a content-free
/// reason (C3), and `hide()` publishes `idle`, zeroes the levels, and clears
/// `ErrorMessage` (P3).
pub struct DbusIndicator {
    bus: SharedBus,
    readiness: Readiness,
    /// Last published `(state, error_message)` — drives the one-signal-per-
    /// transition dedup and the `ErrorMessage` invariant (set iff error).
    last: Option<(String, String)>,
}

impl DbusIndicator {
    /// Publish over `bus`, splitting `recording`/`loading` per `readiness`.
    pub fn new(bus: SharedBus, readiness: Readiness) -> Self {
        Self {
            bus,
            readiness,
            last: None,
        }
    }

    /// Emit the transition (unless it repeats the current wire state) and keep
    /// the `State`/`ErrorMessage` properties consistent with it.
    async fn publish(&mut self, state: &str, error_message: &str) {
        let current = (state.to_string(), error_message.to_string());
        if self.last.as_ref() == Some(&current) {
            return; // idempotent per wire state (Indicator seam contract)
        }
        let leaving_error = matches!(&self.last, Some((s, _)) if s == wire_state::ERROR)
            && state != wire_state::ERROR;
        self.last = Some(current);

        let mut bus = self.bus.lock().await;
        bus.set_property("State", PropertyValue::Str(state.to_string()))
            .await;
        if state == wire_state::ERROR {
            bus.set_property(
                "ErrorMessage",
                PropertyValue::Str(error_message.to_string()),
            )
            .await;
        } else if leaving_error {
            bus.set_property("ErrorMessage", PropertyValue::Str(String::new()))
                .await;
        }
        bus.emit_state_changed(state, error_message).await;
    }
}

#[async_trait]
impl Indicator for DbusIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        let error_message = match &state {
            IndicatorState::Error(msg) => msg.clone(),
            _ => String::new(),
        };
        let wire = map_state(&state, self.readiness.ready_seen());
        self.publish(wire, &error_message).await;
    }

    async fn hide(&mut self) {
        self.publish(wire_state::IDLE, "").await;
        // P3: levels and any error reason die with the session.
        let mut bus = self.bus.lock().await;
        bus.set_property("AudioRms", PropertyValue::F64(0.0)).await;
        bus.set_property("AudioPeak", PropertyValue::F64(0.0)).await;
        bus.set_property("ErrorMessage", PropertyValue::Str(String::new()))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract publisher.md P1 / data-model E1: every IndicatorState maps to
    /// the org.myna.Dictation State string of the E1 table.
    #[test]
    fn indicator_state_maps_to_wire_state() {
        assert_eq!(map_state(&IndicatorState::Hidden, false), "idle");
        assert_eq!(map_state(&IndicatorState::Transcribing, true), "transcribing");
        assert_eq!(map_state(&IndicatorState::Finalizing, true), "finalizing");
        assert_eq!(
            map_state(&IndicatorState::Error("refusing to type".into()), true),
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

    /// C3: the mapping can only emit one of the six contract state strings —
    /// no payload it produces can carry transcript text.
    #[test]
    fn mapping_outputs_are_content_free() {
        const WIRE_STATES: [&str; 6] = [
            "idle",
            "loading",
            "recording",
            "transcribing",
            "finalizing",
            "error",
        ];
        for ready in [false, true] {
            for state in [
                IndicatorState::Hidden,
                IndicatorState::Recording,
                IndicatorState::Transcribing,
                IndicatorState::Finalizing,
                IndicatorState::Error("a transcript would go here".into()),
            ] {
                assert!(WIRE_STATES.contains(&map_state(&state, ready)));
            }
        }
    }

    /// R4: the tracker resets each session — a warm session (`Ready` seen)
    /// followed by a fresh cold session reports `loading` again until the new
    /// `Ready`.
    #[test]
    fn readiness_tracks_loading_then_ready_per_session() {
        let readiness = Readiness::new();
        assert!(!readiness.ready_seen());

        readiness.note_loading();
        assert!(!readiness.ready_seen());
        readiness.note_ready();
        assert!(readiness.ready_seen());

        readiness.reset();
        assert!(!readiness.ready_seen(), "new session starts cold again");
    }

    /// The tee observes `Loading`/`Ready` and forwards everything unchanged.
    #[tokio::test]
    async fn readiness_tee_tracks_liveness_and_forwards() {
        use myna_orchestrator::CollectingSink;

        let readiness = Readiness::new();
        let mut tee = ReadinessTee::new(CollectingSink::default(), readiness.clone());

        tee.emit(OrchestratorEvent::Loading).await;
        assert!(!readiness.ready_seen());
        tee.emit(OrchestratorEvent::Ready).await;
        assert!(readiness.ready_seen());
        tee.emit(OrchestratorEvent::Transcribing).await;
        assert!(readiness.ready_seen(), "unrelated events don't touch it");

        assert_eq!(tee.inner.events.len(), 3, "every event forwarded");
    }
}
