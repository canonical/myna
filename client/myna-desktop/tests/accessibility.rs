//! Hermetic accessibility-wiring tests (feature 011-accessible-dictation-ux,
//! US1, T030/T031). No D-Bus / IBus / portal / display — the real `atspi`
//! bus round-trip is `tests/atspi_hw.rs` (env-gated, T032).
//!
//! Also covers US3's sound-cue controller wiring (T056/T058): the last three
//! tests below are new for this story.

use std::time::{Duration, Instant};

use myna_audio::{AudioFormat, CaptureSource, ScriptedBackend, Step};
use myna_core::SessionConfig;
use myna_desktop::accessibility::fake::Recorded;
use myna_desktop::accessibility::{AnnouncingIndicator, FakeAnnouncer};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::indicator::mock::MockIndicator;
use myna_desktop::inject::mock::MockInjector;
use myna_desktop::sound::{CueKind, FakeSoundCuePlayer, NullSoundCuePlayer};
use myna_desktop::DesktopController;
use myna_orchestrator::{
    run_dictation, FakeBackend, OrchestratorEvent, ScriptedTrigger, StopHandle, TriggerEdge,
};
use tokio::sync::mpsc;

fn silent_source() -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(ScriptedBackend::new(vec![Step::Silence(
            Duration::from_millis(100),
        )])))
        .build()
}

/// A session factory that runs `FakeBackend::commit_drain` over a fresh
/// silent capture source — a real, multi-transition utterance (Recording →
/// Transcribing → Finalizing → Completed), the same fixture
/// `tests/controller.rs`'s T011 uses.
fn commit_drain_session(
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send {
    move |events: mpsc::Sender<OrchestratorEvent>| {
        let backend = FakeBackend::commit_drain();
        let source = silent_source();
        let stop = source.stop_handle();
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await
        });
        (run, stop)
    }
}

// ── T030: every state transition the controller drives also announces ──────

#[tokio::test]
async fn a_full_utterance_announces_each_transition_exactly_once() {
    let fake = FakeAnnouncer::new();
    let announcer_log = fake.log();
    let indicator = AnnouncingIndicator::new(MockIndicator::new(), fake);

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(MockInjector::new())
        .indicator(indicator)
        .session(commit_drain_session())
        .build();

    controller.run().await;

    let announce_texts: Vec<String> = announcer_log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            Recorded::Announce { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    // Recording ("Listening") → Finalizing ("Finishing") → Idle: this fixture
    // never surfaces Transcribing to the indicator (event_to_indicator keeps
    // Transcribing mapped to Recording while the key is still held — see
    // controller.rs's doc comment), and the completed transcript hides the
    // indicator (Idle). Each announced exactly once, no duplicates, no
    // stale re-announcement of a superseded state (Acceptance Scenario 2).
    assert_eq!(announce_texts, vec!["Listening", "Finishing", "Idle"]);
}

// ── T031: adding the announcer changes nothing about the injection/focus
//    path — the announcer/indicator seam has no capability to touch focus,
//    and this proves it introduces no observable side effect on that seam ──

#[tokio::test]
async fn announcing_introduces_no_side_effect_on_the_injection_path() {
    // Baseline: a plain MockIndicator, no announcer at all.
    let baseline_injector = MockInjector::new();
    let baseline_log = baseline_injector.log();
    let mut baseline = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(baseline_injector)
        .indicator(MockIndicator::new())
        .session(commit_drain_session())
        .build();
    baseline.run().await;

    // Same script, wrapped in AnnouncingIndicator.
    let wrapped_injector = MockInjector::new();
    let wrapped_log = wrapped_injector.log();
    let mut wrapped = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(wrapped_injector)
        .indicator(AnnouncingIndicator::new(
            MockIndicator::new(),
            FakeAnnouncer::new(),
        ))
        .session(commit_drain_session())
        .build();
    wrapped.run().await;

    let baseline_log = baseline_log.lock().unwrap();
    let wrapped_log = wrapped_log.lock().unwrap();
    assert_eq!(baseline_log.commits, wrapped_log.commits);
    assert_eq!(baseline_log.preedits, wrapped_log.preedits);
    assert_eq!(baseline_log.acquires, wrapped_log.acquires);
    assert_eq!(baseline_log.releases, wrapped_log.releases);
    // The whole interaction sequence, not just the counts: announcing must not
    // reorder what the target sees either.
    assert_eq!(baseline_log.order, wrapped_log.order);
    assert_eq!(baseline.state(), wrapped.state());
}

// ── T058: a successful utterance plays SessionStart, StopListening, then
//    SessionEnd ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_successful_utterance_plays_session_start_then_session_end() {
    let fake_sound = FakeSoundCuePlayer::new();
    let sound_log = fake_sound.log();

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(MockInjector::new())
        .indicator(MockIndicator::new())
        .session(commit_drain_session())
        .sound(fake_sound)
        .build();

    controller.run().await;

    assert_eq!(
        *sound_log.lock().unwrap(),
        vec![CueKind::SessionStart, CueKind::StopListening, CueKind::SessionEnd],
        "a successful completion plays the start cue, then the immediate stop-listening cue \
         on Release, then the end cue once the result is known — never a Failure cue"
    );
}

// ── T058: a failed utterance plays SessionStart, StopListening, then
//    Failure, never SessionEnd (a genuine error is not a normal session
//    end) — the scripted trigger here has only a `Press` edge, so it "ends"
//    (returns `None` on the next poll) before the backend's own mid-stream
//    error resolves, which legitimately plays StopListening too (capture
//    genuinely stops there, same as a real trigger ending) ────────────────

#[tokio::test]
async fn a_failed_utterance_plays_session_start_then_failure_not_session_end() {
    let fake_sound = FakeSoundCuePlayer::new();
    let sound_log = fake_sound.log();

    let session = move |events: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        let backend = FakeBackend::mid_stream_error("backend_fault", "the backend faulted");
        let source = CaptureSource::builder(AudioFormat::default())
            .backend(Box::new(ScriptedBackend::new(vec![Step::Silence(
                Duration::from_millis(100),
            )])))
            .build();
        let stop = source.stop_handle();
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await
        });
        (run, stop)
    };

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([TriggerEdge::Press]))
        .injector(MockInjector::new())
        .indicator(MockIndicator::new())
        .session(session)
        .sound(fake_sound)
        .build();

    controller.run().await;

    assert_eq!(
        *sound_log.lock().unwrap(),
        vec![CueKind::SessionStart, CueKind::StopListening, CueKind::Failure],
        "a failed utterance plays the start cue, the stop-listening cue once capture ends, \
         then the failure cue — never a session-end cue"
    );
}

// ── T056: the sound-cue integration adds no measurable delay to a full
//    utterance (FR-011). `SoundCuePlayer::play` is a plain, non-`async fn`
//    (see `sound::SoundCuePlayer`'s doc comment) so `controller.rs` cannot
//    `.await` it even by accident — this test is the empirical companion to
//    that structural guarantee, timed the same way `tests/watermarks.rs`'s
//    hermetic per-segment watermark is: a generous tolerance, run offline,
//    that would catch a regression where a future change makes `play()` (or
//    something called from it) block synchronously in the hot path. The
//    *real* `PipeWireSoundCuePlayer`'s own near-instant-return guarantee is
//    exercised separately by `tests/sound_hw.rs`'s env-gated suite (T055).

#[tokio::test]
async fn sound_cue_wiring_adds_no_measurable_delay_to_the_capture_path() {
    const TOLERANCE: Duration = Duration::from_millis(50);

    let mut without_sound = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(MockInjector::new())
        .indicator(MockIndicator::new())
        .session(commit_drain_session())
        .sound(NullSoundCuePlayer)
        .build();
    let start = Instant::now();
    without_sound.run().await;
    let baseline_elapsed = start.elapsed();

    let mut with_sound = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(MockInjector::new())
        .indicator(MockIndicator::new())
        .session(commit_drain_session())
        .sound(FakeSoundCuePlayer::new())
        .build();
    let start = Instant::now();
    with_sound.run().await;
    let with_sound_elapsed = start.elapsed();

    let delta = with_sound_elapsed
        .checked_sub(baseline_elapsed)
        .unwrap_or(Duration::ZERO);
    assert!(
        delta < TOLERANCE,
        "wiring in a sound-cue player added {delta:?}, exceeding the {TOLERANCE:?} tolerance \
         (baseline {baseline_elapsed:?}, with sound {with_sound_elapsed:?})"
    );
}
