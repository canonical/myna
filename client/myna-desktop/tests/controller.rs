//! Hermetic controller tests (no D-Bus / IBus / portal / display).
//!
//! US1 (T011–T016) + T011a: the [`DesktopController`] drives committed
//! transcripts into the injector (commit-only, in order, each once), shows the
//! right indicator states, never captures audio outside a Press→Release window
//! (FR-004/SC-004), and surfaces acquire/stream errors as a clean `Error` state
//! without capturing. All boundaries are mocks; the session runs the *real*
//! `run_dictation` path over `FakeBackend` + a mock capture source, so the
//! capture-at-press / ready-gate reuse is exercised, not re-implemented.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use myna_audio::{AudioFormat, CaptureSource, ScriptedBackend, Step};
use myna_core::{AudioSource, CaptureStream, SessionConfig};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::indicator::mock::MockIndicator;
use myna_desktop::indicator::IndicatorState;
use myna_desktop::inject::mock::{AcquireOutcome, MockInjector};
use myna_desktop::{DesktopController, DictationState};
use myna_orchestrator::{
    run_dictation, FakeBackend, OrchestratorEvent, ScriptedTrigger, SessionOutcome, StopHandle,
    TriggerEdge,
};
use tokio::sync::mpsc;

// ── Session-factory helpers ─────────────────────────────────────────────────

/// A short silent mock capture source (100 ms), in the default format.
fn silent_source() -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(ScriptedBackend::new(vec![Step::Silence(Duration::from_millis(100))])))
        .build()
}

/// A session factory that runs `backend` over a fresh silent capture source,
/// forwarding events on the controller's channel. `wrap` lets a test observe or
/// substitute the audio source (e.g. the T011a capture probe).
fn backend_session(
    make_backend: impl Fn() -> FakeBackend + Send + 'static,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send {
    move |events: mpsc::Sender<OrchestratorEvent>| {
        let backend = make_backend();
        let source = silent_source();
        let stop = source.stop_handle();
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await
        });
        (run, stop)
    }
}

/// A session that emits a fixed `OrchestratorEvent` script and completes with
/// `outcome` (used where no `FakeBackend` fixture matches, e.g. no-speech).
fn events_session(
    events: Vec<OrchestratorEvent>,
    outcome: SessionOutcome,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send {
    let mut slot = Some((events, outcome));
    move |tx: mpsc::Sender<OrchestratorEvent>| {
        let (events, outcome) = slot.take().expect("events_session is single-use");
        let stop = StopHandle::default();
        let run: SessionRun = Box::pin(async move {
            for e in events {
                let _ = tx.send(e).await;
            }
            Ok(outcome)
        });
        (run, stop)
    }
}

fn build(
    edges: impl IntoIterator<Item = TriggerEdge>,
    injector: MockInjector,
    indicator: MockIndicator,
    session: impl myna_desktop::SessionFactory + 'static,
) -> DesktopController {
    DesktopController::builder()
        .trigger(ScriptedTrigger::new(edges))
        .injector(injector)
        .indicator(indicator)
        .session(session)
        .build()
}

// ── T011: commit twice, in order, each once, never re-committed ──────────────

#[tokio::test]
async fn commit_drain_commits_each_segment_once_in_order() {
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        backend_session(FakeBackend::commit_drain),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(log.commits, vec!["the quick brown fox", "jumps over the lazy dog."]);
    // Each segment appears exactly once — never re-committed.
    assert_eq!(log.commits.len(), 2);
    assert_eq!(log.restores, 1);
    assert_eq!(controller.state(), DictationState::Idle);
}

// ── T011a: push-to-talk — no capture while Idle; capture only in a session ────

/// Wraps an [`AudioSource`], recording each `capture()` invocation in a shared
/// probe. Capture *only* happens inside `capture()`, which the controller calls
/// exactly once per started session — so the probe count is the number of
/// capture windows.
struct RecordingSource {
    inner: Box<dyn AudioSource>,
    probe: Arc<Mutex<usize>>,
}

impl AudioSource for RecordingSource {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }
    fn capture(self: Box<Self>) -> CaptureStream {
        *self.probe.lock().unwrap() += 1;
        let this = *self;
        this.inner.capture()
    }
}

#[tokio::test]
async fn no_capture_while_idle_only_between_press_and_release() {
    let probe = Arc::new(Mutex::new(0usize));
    let injector = MockInjector::new();
    let inject_log = injector.log();

    let session_probe = probe.clone();
    let session = move |events: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        let backend = FakeBackend::commit_drain();
        let source = RecordingSource { inner: Box::new(silent_source()), probe: session_probe.clone() };
        // A RecordingSource has no stop_handle of its own; drive the inner one.
        let stop = StopHandle::default();
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await
        });
        (run, stop)
    };

    // Two full push-to-talk cycles.
    let mut controller = build(
        [
            TriggerEdge::Press,
            TriggerEdge::Release,
            TriggerEdge::Press,
            TriggerEdge::Release,
        ],
        injector,
        MockIndicator::new(),
        session,
    );
    controller.run().await;

    // Capture started exactly once per Press — never while Idle, never twice
    // per session, never between sessions (SC-004).
    assert_eq!(*probe.lock().unwrap(), 2, "capture must start once per Press only");
    // Both sessions committed.
    assert_eq!(inject_log.lock().unwrap().commits.len(), 4);
    assert_eq!(controller.state(), DictationState::Idle);
}

// ── T012: cold-load — buffered speech yields exactly one eventual commit ──────

#[tokio::test]
async fn cold_load_yields_exactly_one_commit_nothing_lost() {
    // A cold load (`Loading` before `Ready`) followed by a single committed
    // segment: the controller commits it exactly once and loses nothing. (The
    // ring-buffering of speech across the load window is the orchestrator's
    // capture-at-press behavior, asserted at the capture seam in T011a and in
    // feature 002; here we assert the controller's one-commit outcome.)
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Loading,
                OrchestratorEvent::Ready,
                OrchestratorEvent::Final("late".into()),
                OrchestratorEvent::Done("late".into()),
            ],
            SessionOutcome::Completed { transcript: "late".into() },
        ),
    );
    controller.run().await;

    assert_eq!(inject_log.lock().unwrap().commits, vec!["late"]);
    assert_eq!(controller.state(), DictationState::Idle);
}

// ── T013: an unstable Snippet is never committed (commit-only) ────────────────

#[tokio::test]
async fn snippet_is_never_committed() {
    // happy_path emits snippets ("The quick", "jumps over") *and* finals; only
    // the finals may be committed.
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        backend_session(FakeBackend::happy_path),
    );
    controller.run().await;

    let commits = inject_log.lock().unwrap().commits.clone();
    assert_eq!(commits, vec!["The quick brown fox", "jumps over the lazy dog."]);
    // No committed segment is a bare snippet.
    assert!(!commits.iter().any(|c| c == "The quick" || c == "jumps over"));
}

// ── T014: a no-speech session commits nothing and ends clean ──────────────────

#[tokio::test]
async fn no_speech_session_commits_nothing() {
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Loading,
                OrchestratorEvent::Ready,
                OrchestratorEvent::Done(String::new()),
            ],
            SessionOutcome::Completed { transcript: String::new() },
        ),
    );
    controller.run().await;

    assert!(inject_log.lock().unwrap().commits.is_empty(), "no speech → no commit");
    // Teardown still released the engine cleanly (one restore via end/cancel).
    {
        let log = inject_log.lock().unwrap();
        assert_eq!(log.ends + log.cancels, 1);
    }
    assert_eq!(controller.state(), DictationState::Idle);
}

// ── T015: acquire NoTarget / Unavailable → Error state, no capture ────────────

async fn assert_acquire_error_aborts_without_capture(outcome: AcquireOutcome) {
    let probe = Arc::new(Mutex::new(0usize));
    let injector = MockInjector::new().with_acquires([outcome]);
    let inject_log = injector.log();
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();

    let session_probe = probe.clone();
    let session = move |_events: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        *session_probe.lock().unwrap() += 1; // must never happen
        (Box::pin(async { Ok(SessionOutcome::Aborted) }), StopHandle::default())
    };

    let mut controller =
        build([TriggerEdge::Press], injector, indicator, session);
    controller.run().await;

    assert_eq!(*probe.lock().unwrap(), 0, "no session/capture on an acquire error");
    assert!(inject_log.lock().unwrap().commits.is_empty());
    assert!(
        matches!(indicate_log.lock().unwrap().last(), Some(IndicatorState::Error(_))),
        "acquire error must surface an Error state"
    );
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn no_target_surfaces_error_without_capturing() {
    assert_acquire_error_aborts_without_capture(AcquireOutcome::NoTarget).await;
}

#[tokio::test]
async fn unavailable_surfaces_error_without_capturing() {
    assert_acquire_error_aborts_without_capture(AcquireOutcome::Unavailable("ibus down".into()))
        .await;
}

// ── T016: literal text only; cancel/end idempotent + restore-once on error ────

#[tokio::test]
async fn error_path_cancels_and_restores_exactly_once() {
    // mid_stream_error: some progress then a terminal error instead of done.
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        indicator,
        backend_session(|| FakeBackend::mid_stream_error("decode_failed", "boom")),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    // The engine is restored exactly once even on the error path (I11).
    assert_eq!(log.restores, 1);
    // Only literal transcript text was ever committed — no key-combo tokens.
    for c in &log.commits {
        assert!(!c.contains('\t') && !c.to_lowercase().contains("alt+") && !c.contains("Super"));
    }
    assert!(matches!(indicate_log.lock().unwrap().last(), Some(IndicatorState::Error(_))));
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn mock_injector_cancel_and_end_are_idempotent() {
    // Direct seam check: repeated cancel/end restore the prior engine once.
    use myna_desktop::inject::Injector;
    let mut injector = MockInjector::new();
    let log = injector.log();
    let _ = injector.acquire().await.unwrap();
    injector.cancel().await;
    injector.cancel().await;
    injector.end().await;
    injector.end().await;
    let log = log.lock().unwrap();
    assert_eq!(log.restores, 1, "prior engine restored exactly once");
    assert_eq!(log.cancels, 2);
    assert_eq!(log.ends, 2);
}

// ── Foundational: full lifecycle indicator timeline (kept from 003a) ──────────

#[tokio::test]
async fn full_session_walks_recording_finalizing_hidden() {
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        MockInjector::new(),
        indicator,
        backend_session(FakeBackend::commit_drain),
    );
    controller.run().await;

    let states = indicate_log.lock().unwrap().clone();
    assert_eq!(states.first(), Some(&IndicatorState::Recording));
    assert!(states.contains(&IndicatorState::Finalizing), "expected Finalizing: {states:?}");
    assert_eq!(states.last(), Some(&IndicatorState::Hidden));
    // Privacy (N8): no transcript text leaked into any indicator state.
    for s in &states {
        if let IndicatorState::Error(msg) = s {
            assert!(!msg.contains("quick"), "indicator leaked text: {msg}");
        }
    }
}

// ── T027 [US3]: indicator lifecycle sequence + error, no transcript text ──────

#[tokio::test]
async fn indicator_walks_recording_transcribing_finalizing_hidden() {
    // A realistic push-to-talk: liveness (Loading/Ready/Transcribing) streams
    // while recording, THEN the user releases (→ Finalizing), then the tail
    // final + Done arrive (→ Hidden). The session emits liveness now and the
    // tail only after the release (stop); the controller's biased select drains
    // the buffered liveness before handling the release, so the order is
    // deterministic (no timing).
    let staged = |tx: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        // Pre-buffer the liveness events so the biased select drains them before
        // it sees the (immediate, scripted) Release — deterministic, no timing.
        let _ = tx.try_send(OrchestratorEvent::Loading);
        let _ = tx.try_send(OrchestratorEvent::Ready);
        let _ = tx.try_send(OrchestratorEvent::Transcribing);
        let stop = StopHandle::default();
        let stop2 = stop.clone();
        let run: SessionRun = Box::pin(async move {
            while !stop2.is_stopped() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let _ = tx.send(OrchestratorEvent::Final("hello world".into())).await;
            let _ = tx.send(OrchestratorEvent::Done("hello world".into())).await;
            Ok(SessionOutcome::Completed { transcript: "hello world".into() })
        });
        (run, stop)
    };

    let indicator = MockIndicator::new();
    let log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        MockInjector::new(),
        indicator,
        staged,
    );
    controller.run().await;

    // The distinct states appear in lifecycle order (dedup adjacent repeats).
    let mut seq: Vec<IndicatorState> = Vec::new();
    for s in log.lock().unwrap().iter() {
        if seq.last() != Some(s) {
            seq.push(s.clone());
        }
    }
    assert_eq!(
        seq,
        vec![
            IndicatorState::Recording,
            IndicatorState::Transcribing,
            IndicatorState::Finalizing,
            IndicatorState::Hidden,
        ],
        "indicator lifecycle order"
    );
}

#[tokio::test]
async fn indicator_shows_error_state_on_failure() {
    let indicator = MockIndicator::new();
    let log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        MockInjector::new(),
        indicator,
        backend_session(|| FakeBackend::mid_stream_error("decode_failed", "boom")),
    );
    controller.run().await;

    let states = log.lock().unwrap().clone();
    assert!(
        states.iter().any(|s| matches!(s, IndicatorState::Error(m) if m == "boom")),
        "expected Error(\"boom\"): {states:?}"
    );
}

// ── US4 safety (T032/T033): focus-loss policy ──────────────────────────────

/// A session that pre-buffers `Final("first")` (committed before the focus
/// event) and, after stop, emits `Final("second")` + Done (the tail that must be
/// suppressed once focus is lost).
fn two_segment_focus_session(
    outcome: SessionOutcome,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send {
    move |tx: mpsc::Sender<OrchestratorEvent>| {
        let _ = tx.try_send(OrchestratorEvent::Loading);
        let _ = tx.try_send(OrchestratorEvent::Ready);
        let _ = tx.try_send(OrchestratorEvent::Final("first".into()));
        let stop = StopHandle::default();
        let stop2 = stop.clone();
        let outcome = outcome.clone();
        let run: SessionRun = Box::pin(async move {
            while !stop2.is_stopped() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let _ = tx.send(OrchestratorEvent::Final("second".into())).await;
            let _ = tx.send(OrchestratorEvent::Done("first second".into())).await;
            Ok(outcome)
        });
        (run, stop)
    }
}

#[tokio::test]
async fn focus_out_finalizes_and_makes_no_further_commits() {
    // T032 / I8 / SC-007: focus moves away mid-session — already-committed text
    // stays, but NO further segment is committed (nothing lands in the new
    // surface).
    let injector = MockInjector::new().with_focus_events([myna_desktop::FocusEvent::FocusOut]);
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press],
        injector,
        MockIndicator::new(),
        two_segment_focus_session(SessionOutcome::Completed { transcript: "first second".into() }),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(log.commits, vec!["first"], "no commit after focus-out");
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn target_gone_cancels_and_makes_no_further_commits() {
    // T033 / I9 / FR-022: the target window closes mid-session — discard the
    // uncommitted tail, cancel (restore the engine), and notify (Error state).
    let injector = MockInjector::new().with_focus_events([myna_desktop::FocusEvent::TargetGone]);
    let inject_log = injector.log();
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press],
        injector,
        indicator,
        two_segment_focus_session(SessionOutcome::Completed { transcript: "first second".into() }),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(log.commits, vec!["first"], "no commit after target-gone");
    assert!(log.cancels >= 1, "target-gone must cancel");
    assert_eq!(log.restores, 1, "engine restored exactly once");
    assert!(
        matches!(indicate_log.lock().unwrap().last(), Some(IndicatorState::Error(_))),
        "target-gone must notify"
    );
    assert_eq!(controller.state(), DictationState::Idle);
}
