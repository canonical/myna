//! Sound-cue playback (FR-010/011): short, optional, per-cue-disableable
//! audible cues on session start/end/failure, reusing the vendored `pipewire`
//! crate (research.md R2) rather than adding a new audio-output dependency.
//!
//! Privacy (constitution V): a cue is a fixed, content-free, compile-time-
//! synthesized tone selected only by [`CueKind`] — never derived from, or
//! capable of carrying, transcript text or raw captured audio. The playback
//! path is output-only and shares nothing with the capture buffer.
//!
//! Three pieces, mirroring `accessibility::gate`'s composable-wrapper shape:
//! - [`SoundCuePlayer`] — the seam (`play(&mut self, cue: CueKind)`, plain
//!   `fn`, not `async fn`: a real implementation MUST return near-instantly
//!   — see [`playback::PipeWireSoundCuePlayer`]'s doc comment — so calling it
//!   from `controller.rs`'s hot path can never measurably delay capture or
//!   injection (FR-011, T056).
//! - [`GatedSoundCuePlayer`] — wraps any `SoundCuePlayer` and consults
//!   [`Preferences::sound_cues_enabled`] (and [`Preferences::cue_enabled`])
//!   before forwarding, so a caller with cues off gets a silent no-op with no
//!   dependency on which backend is wrapped.
//! - [`FakeSoundCuePlayer`] — the hermetic test double, following the
//!   `FakeAnnouncer`/`MockIndicator` convention exactly (a `.log()` accessor
//!   over a shared `Arc<Mutex<Vec<CueKind>>>`, not a public field).
//!
//! **Known scope gap (T054, FR-010 "individually disableable")**:
//! `Preferences::sound_cues_enabled()` is a single global boolean today, not
//! a per-cue one. [`Preferences::cue_enabled`] is the seam a future
//! per-`CueKind` GSettings key would hang off — it defaults to `true`
//! ("no per-cue override yet") on every existing implementor, so adding a
//! real per-cue key later is additive, not a breaking API change. Wiring an
//! actual per-cue GSettings schema key is deliberately NOT done here: it is a
//! larger, separate schema change out of T054's scope.

pub mod playback;

pub use playback::PipeWireSoundCuePlayer;

use std::sync::{Arc, Mutex};

use crate::preferences::Preferences;

/// The four sound cues FR-010 names: session start, stop listening, session
/// end, and failure. Content-free by construction — a fixed enum, never a
/// string that could carry transcript text (constitution V).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CueKind {
    SessionStart,
    /// Played the moment listening stops (Release/FocusOut/trigger-ended),
    /// before the eventual outcome (`SessionEnd`/`Failure`) is known —
    /// distinct from both, so a non-visual user gets an immediate
    /// "I heard you, now processing" signal rather than waiting in silence
    /// for however long transcription takes (manual test report,
    /// 2026-09-01: "I think there should probably be a beep when it stops
    /// listening").
    StopListening,
    SessionEnd,
    Failure,
}

/// The sound-cue playback seam (mirrors `crate::indicator::Indicator` and
/// `crate::accessibility::AccessibilityAnnouncer`'s shape: a small trait any
/// caller can hold as `Box<dyn SoundCuePlayer>`).
///
/// Deliberately a plain, synchronous `fn`, not `async fn`: `controller.rs`
/// calls this inline (not `.await`ed) from `run_one_utterance`'s hot path, so
/// the *type itself* makes an accidental blocking `.await` on cue playback
/// impossible to introduce there — see `playback::PipeWireSoundCuePlayer` for
/// how the real implementation still returns near-instantly despite this
/// (FR-011, T056).
pub trait SoundCuePlayer: Send {
    fn play(&mut self, cue: CueKind);
}

/// The default `SoundCuePlayer` for a `DesktopController` that never opted
/// into sound cues (e.g. every builder call site that predates this
/// feature): a silent no-op. Distinct from "cues disabled by preference"
/// (that's `GatedSoundCuePlayer` + `Preferences::sound_cues_enabled() ==
/// false`) — this is "no player was configured at all".
#[derive(Debug, Default)]
pub struct NullSoundCuePlayer;

impl SoundCuePlayer for NullSoundCuePlayer {
    fn play(&mut self, _cue: CueKind) {}
}

/// Records every `play()` call; never touches real audio output. Contract
/// tests exercise this directly; the shared `.log()` handle follows the
/// `FakeAnnouncer`/`MockIndicator` convention (clone the log before moving
/// the fake into a wrapper/controller).
#[derive(Debug, Default)]
pub struct FakeSoundCuePlayer {
    calls: Arc<Mutex<Vec<CueKind>>>,
}

impl FakeSoundCuePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle to the recorded call sequence.
    pub fn log(&self) -> Arc<Mutex<Vec<CueKind>>> {
        self.calls.clone()
    }
}

impl SoundCuePlayer for FakeSoundCuePlayer {
    fn play(&mut self, cue: CueKind) {
        self.calls.lock().unwrap().push(cue);
    }
}

/// Wraps any `SoundCuePlayer` behind the `Preferences::sound_cues_enabled()`
/// gate (plus the per-cue override seam, `Preferences::cue_enabled` —
/// see the module doc's "known scope gap"). Mirrors
/// `accessibility::gate::VerbosityGatedAnnouncer`'s shape exactly: a plain
/// wrapper that is itself a `SoundCuePlayer`, so it composes without the
/// caller (`controller.rs`) needing to know a gate exists.
pub struct GatedSoundCuePlayer<S, P> {
    inner: S,
    preferences: P,
}

impl<S, P> GatedSoundCuePlayer<S, P> {
    pub fn new(inner: S, preferences: P) -> Self {
        Self { inner, preferences }
    }
}

impl<S, P> SoundCuePlayer for GatedSoundCuePlayer<S, P>
where
    S: SoundCuePlayer,
    P: Preferences,
{
    fn play(&mut self, cue: CueKind) {
        if self.preferences.sound_cues_enabled() && self.preferences.cue_enabled(cue) {
            self.inner.play(cue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::{Preferences, Verbosity};

    /// A fixed-answer `Preferences` stub (mirrors `gate.rs`'s `FixedVerbosity`
    /// test doubles) — only `sound_cues_enabled()` varies; the other two
    /// methods are unused by these tests and return arbitrary fixed values.
    struct FixedSoundPreferences(bool);

    impl Preferences for FixedSoundPreferences {
        fn verbosity(&self) -> Verbosity {
            Verbosity::AllTransitions
        }

        fn sound_cues_enabled(&self) -> bool {
            self.0
        }
    }

    // ── T054: play() is recorded when sound_cues_enabled() is true ─────────

    #[test]
    fn play_is_recorded_when_sound_cues_are_enabled() {
        let fake = FakeSoundCuePlayer::new();
        let log = fake.log();
        let mut gated = GatedSoundCuePlayer::new(fake, FixedSoundPreferences(true));

        gated.play(CueKind::SessionStart);

        assert_eq!(*log.lock().unwrap(), vec![CueKind::SessionStart]);
    }

    // ── T054: play() is a silent no-op when sound_cues_enabled() is false ──

    #[test]
    fn play_is_a_silent_noop_when_sound_cues_are_disabled() {
        let fake = FakeSoundCuePlayer::new();
        let log = fake.log();
        let mut gated = GatedSoundCuePlayer::new(fake, FixedSoundPreferences(false));

        gated.play(CueKind::Failure);

        assert!(log.lock().unwrap().is_empty());
    }
}
