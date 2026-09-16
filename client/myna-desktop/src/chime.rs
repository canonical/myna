//! Dictation chimes (STT UX spec, "State cues: visual indicators and sound") —
//! an [`Indicator`] decorator, [`ChimingIndicator`], that plays a short chime
//! alongside whatever the wrapped indicator does: a "start" chime the first
//! time a session becomes active (`Recording`/`Transcribing`), a "stop" chime
//! when it leaves that active phase (`Finalizing`, or straight to `Hidden` on
//! an abrupt cancel), and an "error" chime on *any* [`IndicatorState::Error`]
//! — recoverable or not. "No speech detected"/"Focus lost" are as much
//! "nothing got typed" as a hard failure is, and a user relying on the
//! chimes (rather than reading the indicator text) needs the same audible
//! cue for all of them; only a `Hidden` that follows a real committed
//! transcript stays silent beyond the `Stop` chime already played at
//! `Finalizing`.
//!
//! [`ChimePlayer`] is the playback seam: [`PipeWireChimePlayer`] is the
//! shipped implementation (`myna_audio::playback`); [`mock::MockChimePlayer`]
//! is the hermetic test fixture. Chime playback is fire-and-forget — a
//! failed or slow chime must never delay `set_state`/`hide`, which
//! `myna-shell`/D-Bus/injection latency budgets assume are cheap.

pub mod mock;

use crate::indicator::{Indicator, IndicatorState};
use async_trait::async_trait;

/// The three chimes the spec's state-cue table calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chime {
    /// Recording became active.
    Start,
    /// Recording finished (finalizing, or an abrupt cancel).
    Stop,
    /// An unrecoverable error.
    Error,
}

/// The chime-playback seam.
pub trait ChimePlayer: Send {
    fn play(&mut self, chime: Chime);
}

/// Regenerate from the design source with:
/// `ffmpeg -i client/data/sounds/<name>.oga -f s16le -ac 1 -ar 44100 client/myna-desktop/assets/sounds/<name>.pcm`
const START_PCM: &[u8] = include_bytes!("../assets/sounds/start.pcm");
const STOP_PCM: &[u8] = include_bytes!("../assets/sounds/stop.pcm");
const ERROR_PCM: &[u8] = include_bytes!("../assets/sounds/error.pcm");
const CHIME_RATE_HZ: u32 = 44100;

fn clip(chime: Chime) -> myna_audio::Clip {
    let samples = match chime {
        Chime::Start => START_PCM,
        Chime::Stop => STOP_PCM,
        Chime::Error => ERROR_PCM,
    };
    myna_audio::Clip {
        samples,
        rate_hz: CHIME_RATE_HZ,
    }
}

/// The shipped [`ChimePlayer`]: native PipeWire playback of the embedded PCM.
#[derive(Default)]
pub struct PipeWireChimePlayer;

impl ChimePlayer for PipeWireChimePlayer {
    fn play(&mut self, chime: Chime) {
        myna_audio::play_clip(clip(chime));
    }
}

/// The coarse phase a displayed [`IndicatorState`] represents, for edge
/// detection — several `IndicatorState` variants (`Recording`/`Transcribing`)
/// are one "listening" phase for chiming purposes. `Notice` covers every
/// `Error` state, recoverable or not: "no speech detected" and "focus lost"
/// are as much "nothing got typed" as an unrecoverable failure is, and a
/// user relying on the chimes (not the visible indicator) needs the same cue
/// for all of them — only a real `Hidden` (a transcript was actually
/// committed) is silent beyond the `Stop`/`Finalizing` chime already played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Listening,
    Finalizing,
    Notice,
}

fn phase_of(state: &IndicatorState) -> Phase {
    match state {
        IndicatorState::Hidden => Phase::Idle,
        IndicatorState::Recording | IndicatorState::Transcribing => Phase::Listening,
        IndicatorState::Finalizing => Phase::Finalizing,
        IndicatorState::Error { .. } => Phase::Notice,
    }
}

/// Which chime, if any, `from → to` should play. `None` covers every
/// idempotent re-set (e.g. repeated `Recording` pings) and every transition
/// the spec's table has no chime for (e.g. into a `Hidden` that followed
/// `Finalizing` — a normal completed transcript).
fn chime_for(from: Phase, to: Phase) -> Option<Chime> {
    match (from, to) {
        (Phase::Listening, Phase::Listening) => None,
        (_, Phase::Listening) => Some(Chime::Start),
        (Phase::Listening, Phase::Finalizing) => Some(Chime::Stop),
        (Phase::Listening, Phase::Idle) => Some(Chime::Stop),
        (_, Phase::Notice) if from != Phase::Notice => Some(Chime::Error),
        _ => None,
    }
}

/// An [`Indicator`] that plays a [`ChimePlayer`] chime alongside whatever the
/// wrapped indicator renders, per [`chime_for`]. Delegates every call
/// unchanged — this only observes the state sequence, never alters it.
///
/// `enabled` is a [`Live<bool>`] (the `chimes-enabled` setting) rather than a
/// plain `bool` so toggling it in Myna Settings takes effect on the very next
/// state change, not at the next daemon restart — the same live-reload
/// contract `preedit`/`auto_stop` already get. The phase is still tracked
/// while muted, so re-enabling mid-session does not misfire on the next
/// transition.
pub struct ChimingIndicator<I, P> {
    inner: I,
    player: P,
    phase: Phase,
    enabled: crate::live::Live<bool>,
}

impl<I: Indicator, P: ChimePlayer> ChimingIndicator<I, P> {
    pub fn new(inner: I, player: P, enabled: crate::live::Live<bool>) -> Self {
        Self {
            inner,
            player,
            phase: Phase::Idle,
            enabled,
        }
    }

    fn observe(&mut self, state: &IndicatorState) {
        let to = phase_of(state);
        if self.enabled.get() {
            if let Some(chime) = chime_for(self.phase, to) {
                self.player.play(chime);
            }
        }
        self.phase = to;
    }
}

#[async_trait]
impl<I: Indicator, P: ChimePlayer> Indicator for ChimingIndicator<I, P> {
    async fn set_state(&mut self, state: IndicatorState) {
        self.observe(&state);
        self.inner.set_state(state).await;
    }

    async fn hide(&mut self) {
        self.observe(&IndicatorState::Hidden);
        self.inner.hide().await;
    }

    async fn set_audio_drops(&mut self, not_resident: u64, not_active: u64) {
        self.inner.set_audio_drops(not_resident, not_active).await;
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockChimePlayer;
    use super::*;
    use crate::indicator::mock::MockIndicator;
    use crate::live::Live;

    fn recoverable() -> IndicatorState {
        IndicatorState::recoverable("no speech detected")
    }

    fn critical() -> IndicatorState {
        IndicatorState::critical("boom")
    }

    fn chiming(player: MockChimePlayer) -> ChimingIndicator<MockIndicator, MockChimePlayer> {
        ChimingIndicator::new(MockIndicator::new(), player, Live::new(true))
    }

    #[tokio::test]
    async fn chimes_start_once_then_stop_on_finalizing() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        chiming.set_state(IndicatorState::Transcribing).await;
        chiming.set_state(IndicatorState::Recording).await;
        chiming.set_state(IndicatorState::Finalizing).await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Stop]);
    }

    #[tokio::test]
    async fn direct_cancel_without_finalizing_still_stops() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        // No Finalizing in between and no Error either — a clean abrupt end
        // straight to Hidden.
        chiming.set_state(IndicatorState::Hidden).await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Stop]);
    }

    #[tokio::test]
    async fn finalizing_does_not_double_stop_on_the_terminal_resting_state() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        chiming.set_state(IndicatorState::Finalizing).await;
        chiming.set_state(IndicatorState::Hidden).await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Stop]);
    }

    #[tokio::test]
    async fn recoverable_notice_chimes_error_in_addition_to_the_stop_already_played() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        chiming.set_state(IndicatorState::Finalizing).await;
        // "No speech detected" — nothing got typed, same as a hard failure
        // from the chime's point of view (see the module doc comment).
        chiming.set_state(recoverable()).await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Stop, Chime::Error]);
    }

    #[tokio::test]
    async fn a_critical_error_straight_from_listening_skips_the_stop_chime() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        // No Finalizing in between (e.g. the dictation target closed).
        chiming.set_state(critical()).await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Error]);
    }

    #[tokio::test]
    async fn pre_capture_abort_chimes_error_only_no_start_or_stop() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(critical()).await;

        assert_eq!(player.log(), vec![Chime::Error]);
    }

    #[tokio::test]
    async fn hide_is_equivalent_to_a_resting_state() {
        let player = MockChimePlayer::new();
        let mut chiming = chiming(player.clone());

        chiming.set_state(IndicatorState::Recording).await;
        chiming.hide().await;

        assert_eq!(player.log(), vec![Chime::Start, Chime::Stop]);
    }

    #[tokio::test]
    async fn disabling_mutes_playback_without_losing_phase_tracking() {
        let player = MockChimePlayer::new();
        let enabled = Live::new(true);
        let mut chiming =
            ChimingIndicator::new(MockIndicator::new(), player.clone(), enabled.clone());

        chiming.set_state(IndicatorState::Recording).await;
        enabled.set(false);
        // Muted mid-session: Finalizing plays nothing...
        chiming.set_state(IndicatorState::Finalizing).await;
        enabled.set(true);
        // ...but phase tracking kept up, so re-enabling doesn't replay Stop or
        // misfire Start on the next real transition.
        chiming.set_state(IndicatorState::Hidden).await;

        assert_eq!(player.log(), vec![Chime::Start]);
    }
}
