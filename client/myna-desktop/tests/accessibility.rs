//! Hermetic accessibility-wiring tests (feature 011-accessible-dictation-ux,
//! US1, T030/T031). No D-Bus / IBus / portal / display — the real `atspi`
//! bus round-trip is `tests/atspi_hw.rs` (env-gated, T032).

use std::time::Duration;

use myna_audio::{AudioFormat, CaptureSource, ScriptedBackend, Step};
use myna_core::SessionConfig;
use myna_desktop::accessibility::fake::Recorded;
use myna_desktop::accessibility::{AnnouncingIndicator, FakeAnnouncer};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::indicator::mock::MockIndicator;
use myna_desktop::inject::mock::MockInjector;
use myna_desktop::DesktopController;
use myna_orchestrator::{run_dictation, FakeBackend, OrchestratorEvent, ScriptedTrigger, StopHandle, TriggerEdge};
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
    assert_eq!(baseline_log.acquires, wrapped_log.acquires);
    assert_eq!(baseline_log.restores, wrapped_log.restores);
    assert_eq!(baseline_log.cancels, wrapped_log.cancels);
    assert_eq!(baseline_log.ends, wrapped_log.ends);
    assert_eq!(baseline_log.activity, wrapped_log.activity);
    assert_eq!(baseline.state(), wrapped.state());
}
