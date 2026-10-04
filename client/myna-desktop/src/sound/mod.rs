//! Audible session cues: a sound when dictation starts listening, when it
//! stops, and when it fails, behind the `sounds` setting.
//!
//! The cues are derived from the indicator timeline rather than threaded
//! through the controller: [`Chiming`] wraps whatever [`Indicator`] the daemon
//! runs and hears every state it is shown, so what the user hears can never
//! disagree with what the HUD shows. [`Chime`] is the port;
//! [`player::Player`] plays Myna's own sounds.

use async_trait::async_trait;

use crate::indicator::{Indicator, IndicatorState};
use crate::live::Live;

pub mod player;

/// The three moments a session is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    Start,
    Stop,
    Error,
}

/// Plays a cue. Must return at once: the controller awaits the indicator.
pub trait Chime: Send {
    fn play(&self, cue: Cue);
}

/// Where the session is, as far as the cues care.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Listening,
    /// Stopped listening, the transcript still to come.
    Ending,
    /// A critical error is showing.
    Failed,
}

/// The cue one indicator state earns, and the phase it leaves behind.
///
/// Start once per session, when listening begins: at the press, not when the
/// model is ready, as capture buffers through a cold load. Stop once, at
/// whichever comes first of the end of listening (`Finalizing`) and the end of the
/// session. A recoverable notice ("No speech detected") is an ordinary end.
/// A critical error always sounds, even after Stop, but a repeat of the one
/// already showing does not.
fn step(phase: Phase, state: &IndicatorState) -> (Phase, Option<Cue>) {
    match state {
        IndicatorState::Recording | IndicatorState::Transcribing if phase == Phase::Listening => {
            (Phase::Listening, None)
        }
        IndicatorState::Recording => (Phase::Listening, Some(Cue::Start)),
        IndicatorState::Transcribing => (phase, None),
        IndicatorState::Finalizing => match phase {
            Phase::Listening => (Phase::Ending, Some(Cue::Stop)),
            other => (other, None),
        },
        IndicatorState::Error {
            recoverable: false, ..
        } => (
            Phase::Failed,
            (phase != Phase::Failed).then_some(Cue::Error),
        ),
        IndicatorState::Hidden | IndicatorState::Error { .. } => (
            Phase::Idle,
            (phase == Phase::Listening).then_some(Cue::Stop),
        ),
    }
}

/// An [`Indicator`] that also plays the session's cues while `enabled` holds.
pub struct Chiming<I> {
    inner: I,
    chime: Box<dyn Chime>,
    enabled: Live<bool>,
    phase: Phase,
}

impl<I: Indicator> Chiming<I> {
    pub fn new(inner: I, chime: impl Chime + 'static, enabled: Live<bool>) -> Self {
        Self {
            inner,
            chime: Box::new(chime),
            enabled,
            phase: Phase::Idle,
        }
    }
}

#[async_trait]
impl<I: Indicator> Indicator for Chiming<I> {
    async fn set_state(&mut self, state: IndicatorState) {
        let (phase, cue) = step(self.phase, &state);
        self.phase = phase;
        if let Some(cue) = cue.filter(|_| self.enabled.get()) {
            self.chime.play(cue);
        }
        self.inner.set_state(state).await;
    }

    async fn set_audio_drops(&mut self, not_active: u64) {
        self.inner.set_audio_drops(not_active).await;
    }

    async fn set_last_error(&mut self, headline: &str, detail: &str) {
        self.inner.set_last_error(headline, detail).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::indicator::mock::MockIndicator;

    #[derive(Clone, Default)]
    struct Heard(Arc<Mutex<Vec<Cue>>>);

    impl Chime for Heard {
        fn play(&self, cue: Cue) {
            self.0.lock().unwrap().push(cue);
        }
    }

    fn heard(states: &[IndicatorState]) -> Vec<Cue> {
        let mut phase = Phase::Idle;
        states
            .iter()
            .filter_map(|state| {
                let (next, cue) = step(phase, state);
                phase = next;
                cue
            })
            .collect()
    }

    use IndicatorState::{Finalizing, Hidden, Recording, Transcribing};

    fn critical() -> IndicatorState {
        IndicatorState::critical("backend gone")
    }

    fn notice() -> IndicatorState {
        IndicatorState::recoverable("No speech detected")
    }

    #[test]
    fn a_session_starts_and_stops_once() {
        assert_eq!(
            heard(&[
                Recording,
                Transcribing,
                Recording,
                Transcribing,
                Finalizing,
                Hidden
            ]),
            [Cue::Start, Cue::Stop]
        );
    }

    #[test]
    fn a_session_that_ends_without_finalizing_still_stops() {
        assert_eq!(heard(&[Recording, Hidden]), [Cue::Start, Cue::Stop]);
        assert_eq!(heard(&[Recording, notice()]), [Cue::Start, Cue::Stop]);
    }

    #[test]
    fn a_notice_after_the_stop_is_not_a_second_stop() {
        assert_eq!(
            heard(&[Recording, Finalizing, notice(), Hidden]),
            [Cue::Start, Cue::Stop]
        );
    }

    #[test]
    fn a_failure_sounds_even_after_the_stop_and_only_once() {
        assert_eq!(
            heard(&[Recording, Finalizing, critical(), critical(), Hidden]),
            [Cue::Start, Cue::Stop, Cue::Error]
        );
        assert_eq!(heard(&[Recording, critical()]), [Cue::Start, Cue::Error]);
    }

    #[test]
    fn a_press_refused_before_capture_is_an_error_alone() {
        assert_eq!(heard(&[critical()]), [Cue::Error]);
    }

    #[test]
    fn the_next_session_starts_again_after_any_ending() {
        assert_eq!(
            heard(&[Recording, Hidden, Recording, Finalizing, Recording]),
            [Cue::Start, Cue::Stop, Cue::Start, Cue::Stop, Cue::Start]
        );
        assert_eq!(heard(&[critical(), Recording]), [Cue::Error, Cue::Start]);
    }

    #[test]
    fn a_stray_state_while_idle_is_silent() {
        assert_eq!(heard(&[Hidden, Transcribing, Finalizing, notice()]), []);
    }

    #[derive(Default)]
    struct Drops(Arc<Mutex<Vec<u64>>>);

    #[async_trait]
    impl Indicator for Drops {
        async fn set_state(&mut self, _state: IndicatorState) {}

        async fn set_audio_drops(&mut self, not_active: u64) {
            self.0.lock().unwrap().push(not_active);
        }
    }

    #[tokio::test]
    async fn audio_drops_reach_the_wrapped_indicator() {
        let drops = Drops::default();
        let seen = drops.0.clone();
        let mut chiming = Chiming::new(drops, Heard::default(), Live::new(true));
        chiming.set_audio_drops(7).await;
        assert_eq!(*seen.lock().unwrap(), [7]);
    }

    #[tokio::test]
    async fn the_indicator_sees_every_state_and_the_setting_gates_only_the_sound() {
        let indicator = MockIndicator::new();
        let shown = indicator.log();
        let chime = Heard::default();
        let enabled = Live::new(true);
        let mut chiming = Chiming::new(indicator, chime.clone(), enabled.clone());

        chiming.set_state(Recording).await;
        enabled.set(false);
        chiming.set_state(Finalizing).await;
        chiming.set_state(Hidden).await;
        enabled.set(true);
        chiming.set_state(Recording).await;

        assert_eq!(
            *shown.lock().unwrap(),
            [Recording, Finalizing, Hidden, Recording]
        );
        assert_eq!(*chime.0.lock().unwrap(), [Cue::Start, Cue::Start]);
    }
}
