//! Hermetic controller tests (no D-Bus / IBus / portal / display).
//!
//! Foundational coverage (T010 checkpoint): the [`DesktopController`] composes
//! three mocked boundaries and runs a full mocked session — commits land in
//! order, the indicator walks Recording→Finalizing→Hidden, and the controller
//! returns to `Idle`. The deep US1 guarantees (commit-once, snippet-never,
//! no-capture-between-sessions, cold-load, error states) land here in branch
//! 003b (T011–T017).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use myna_audio::{AudioFormat, CaptureSource, ScriptedBackend, Step};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::indicator::mock::MockIndicator;
use myna_desktop::indicator::IndicatorState;
use myna_desktop::inject::mock::MockInjector;
use myna_desktop::{DesktopController, DictationState};
use myna_orchestrator::{
    run_dictation, FakeBackend, OrchestratorEvent, ScriptedTrigger, StopHandle, TriggerEdge,
};
use myna_core::SessionConfig;
use tokio::sync::mpsc;

/// A session factory that runs `FakeBackend::commit_drain` over a short silent
/// mock capture source — the hermetic analogue of the real capture+inference
/// path. The tail final + `done` arrive only after `session.finish` (i.e. after
/// the controller stops capture on Release), exercising the commit-drain window.
fn commit_drain_session()
-> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send {
    move |events: mpsc::Sender<OrchestratorEvent>| {
        let backend = FakeBackend::commit_drain();
        let source = CaptureSource::builder(AudioFormat::default())
            .backend(Box::new(ScriptedBackend::new(vec![Step::Silence(
                Duration::from_millis(200),
            )])))
            .build();
        let stop = source.stop_handle();
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await
        });
        (run, stop)
    }
}

#[tokio::test]
async fn full_mocked_session_commits_in_order_and_returns_to_idle() {
    let injector = MockInjector::new();
    let indicator = MockIndicator::new();
    let inject_log = injector.log();
    let indicate_log: Arc<Mutex<Vec<IndicatorState>>> = indicator.log();

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([TriggerEdge::Press, TriggerEdge::Release]))
        .injector(injector)
        .indicator(indicator)
        .session(commit_drain_session())
        .build();

    controller.run().await;

    // Committed transcript, in order, each once (commit-only, commit-drain tail).
    let commits = inject_log.lock().unwrap().commits.clone();
    assert_eq!(commits, vec!["the quick brown fox", "jumps over the lazy dog."]);

    // Idempotent teardown restored the engine exactly once (I11).
    assert_eq!(inject_log.lock().unwrap().restores, 1);

    // The indicator became active, passed through Finalizing, and cleared.
    let states = indicate_log.lock().unwrap().clone();
    assert_eq!(states.first(), Some(&IndicatorState::Recording));
    assert!(states.contains(&IndicatorState::Finalizing), "expected a Finalizing state: {states:?}");
    assert_eq!(states.last(), Some(&IndicatorState::Hidden));

    // No transcript text ever reached the indicator (privacy, N8).
    for s in &states {
        if let IndicatorState::Error(msg) = s {
            assert!(msg.is_empty() || !msg.contains("quick"), "indicator leaked text: {msg}");
        }
    }

    // Back to Idle at the end of the loop.
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn secure_field_refuses_before_any_capture() {
    // acquire() → Err(SecureField): the controller shows an error and never
    // starts a session (a slice of US4/T034, exercised via the mock here to
    // prove the pre-capture abort path in the foundational controller).
    use myna_desktop::inject::mock::AcquireOutcome;

    let injector = MockInjector::new().with_acquires([AcquireOutcome::Secure]);
    let indicator = MockIndicator::new();
    let inject_log = injector.log();
    let indicate_log = indicator.log();

    // A session factory that would panic if ever started — it must not be.
    let never = |_events: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        panic!("secure field must not start a capture session");
    };

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([TriggerEdge::Press]))
        .injector(injector)
        .indicator(indicator)
        .session(never)
        .build();

    controller.run().await;

    assert!(inject_log.lock().unwrap().commits.is_empty());
    let states = indicate_log.lock().unwrap().clone();
    assert!(
        matches!(states.last(), Some(IndicatorState::Error(_))),
        "expected an error state, got {states:?}"
    );
    assert_eq!(controller.state(), DictationState::Idle);
}
