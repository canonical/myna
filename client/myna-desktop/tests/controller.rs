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
use myna_desktop::{DesktopController, DictationState, Live};
use myna_orchestrator::{
    run_dictation, FakeBackend, OrchestratorEvent, ScriptedTrigger, SessionOutcome, StopHandle,
    TriggerEdge,
};
use tokio::sync::mpsc;

// ── Session-factory helpers ─────────────────────────────────────────────────

/// A short silent mock capture source (100 ms), in the default format.
fn silent_source() -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(ScriptedBackend::new(vec![Step::Silence(
            Duration::from_millis(100),
        )])))
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
    // Coalesced: the two finals arrive back-to-back (no event between them), so
    // they are inserted as ONE CommitText — rapid successive IBus commits race
    // and only the last lands, so the burst is joined. Order + content preserved.
    assert_eq!(
        log.commits,
        vec!["the quick brown fox jumps over the lazy dog."]
    );
    assert_eq!(log.commits.len(), 1);
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
        let source = RecordingSource {
            inner: Box::new(silent_source()),
            probe: session_probe.clone(),
        };
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
    assert_eq!(
        *probe.lock().unwrap(),
        2,
        "capture must start once per Press only"
    );
    // Both sessions committed (one coalesced commit each — see
    // `commit_drain_commits_each_segment_once_in_order`).
    assert_eq!(inject_log.lock().unwrap().commits.len(), 2);
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
            SessionOutcome::Completed {
                transcript: "late".into(),
            },
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
    // Two spaced flushes (a progress event separates the finals); the second
    // carries the defensive separator — verbatim concatenation reconstructs
    // the transcript.
    assert_eq!(
        commits,
        vec!["The quick brown fox", " jumps over the lazy dog."]
    );
    assert_eq!(
        commits.concat(),
        "The quick brown fox jumps over the lazy dog."
    );
    // No committed segment is a bare snippet.
    assert!(!commits
        .iter()
        .any(|c| c == "The quick" || c == "jumps over"));
}

// ── T014: a no-speech session commits nothing and ends clean ──────────────────

#[tokio::test]
async fn no_speech_session_commits_nothing() {
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        indicator,
        events_session(
            vec![
                OrchestratorEvent::Loading,
                OrchestratorEvent::Ready,
                OrchestratorEvent::Done(String::new()),
            ],
            SessionOutcome::Completed {
                transcript: String::new(),
            },
        ),
    );
    controller.run().await;

    assert!(
        inject_log.lock().unwrap().commits.is_empty(),
        "no speech → no commit"
    );
    // Teardown still released the engine cleanly (one restore via end/cancel).
    {
        let log = inject_log.lock().unwrap();
        assert_eq!(log.ends + log.cancels, 1);
    }
    assert_eq!(controller.state(), DictationState::Idle);

    // T015/C11 (2026-07-30): the live `Done("")` event and the finalize-block
    // `SessionOutcome::Completed{transcript: ""}` both route through
    // `completion_indicator_state`, so the final indicator state is the
    // recoverable notice — never `Hidden` — and the two calls agree (the
    // second is a no-op under the real DbusIndicator's dedup; here with
    // MockIndicator we just assert the final state is right).
    assert_eq!(
        indicate_log.lock().unwrap().last(),
        Some(&IndicatorState::recoverable("No speech detected")),
        "an empty-transcript completion must surface the recoverable notice, not Hidden"
    );
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
        (
            Box::pin(async { Ok(SessionOutcome::Aborted) }),
            StopHandle::default(),
        )
    };

    let mut controller = build([TriggerEdge::Press], injector, indicator, session);
    controller.run().await;

    assert_eq!(
        *probe.lock().unwrap(),
        0,
        "no session/capture on an acquire error"
    );
    assert!(inject_log.lock().unwrap().commits.is_empty());
    assert!(
        matches!(
            indicate_log.lock().unwrap().last(),
            Some(IndicatorState::Error { .. })
        ),
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
    assert!(matches!(
        indicate_log.lock().unwrap().last(),
        Some(IndicatorState::Error { .. })
    ));
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
    assert!(
        states.contains(&IndicatorState::Finalizing),
        "expected Finalizing: {states:?}"
    );
    assert_eq!(states.last(), Some(&IndicatorState::Hidden));
    // Privacy (N8): no transcript text leaked into any indicator state.
    for s in &states {
        if let IndicatorState::Error { message, .. } = s {
            assert!(
                !message.contains("quick"),
                "indicator leaked text: {message}"
            );
        }
    }
}

// ── T027 [US3]: indicator lifecycle sequence + error, no transcript text ──────

#[tokio::test]
async fn indicator_walks_recording_finalizing_hidden() {
    // A realistic push-to-talk: liveness (Loading/Ready/Transcribing) streams
    // during the session — all shown as Recording (listening), because a
    // `transcribing` event while the user is still speaking must NOT flip the
    // indicator to the "working" look — THEN the user releases (→ Finalizing),
    // then the tail final + Done arrive (→ Hidden). The session emits liveness
    // now and the tail only after the release (stop); the controller's biased
    // select drains the buffered liveness before handling the release, so the
    // order is deterministic (no timing).
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
            let _ = tx
                .send(OrchestratorEvent::Final("hello world".into()))
                .await;
            let _ = tx.send(OrchestratorEvent::Done("hello world".into())).await;
            Ok(SessionOutcome::Completed {
                transcript: "hello world".into(),
            })
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
            IndicatorState::Finalizing,
            IndicatorState::Hidden,
        ],
        "indicator lifecycle order (Transcribing stays on Recording during capture)"
    );
}

/// Regression (manual test report, 2026-07-31; found while fixing the
/// focus-loss bugs — confirmed present even in the pre-2026-07-31 code, so
/// independent of those fixes): a `Transcribing` liveness ping that lands in
/// the event channel just AFTER the Release edge has already moved the
/// controller into `Finalizing` must NOT clobber the indicator back to
/// `Recording`. Unlike `indicator_walks_recording_finalizing_hidden` (which
/// buffers `Transcribing` *before* the Release so it's drained first), this
/// session only sends `Transcribing` once it observes `stop.stop()` having
/// already fired — i.e. strictly after the controller has processed Release
/// and advanced to `Finalizing` — reproducing the real race against a live
/// whisper adapter, which was producing a spurious `finalizing → recording →
/// idle` flicker at the end of every utterance.
#[tokio::test]
async fn late_transcribing_event_after_release_does_not_reopen_recording() {
    let staged = |tx: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        let _ = tx.try_send(OrchestratorEvent::Loading);
        let _ = tx.try_send(OrchestratorEvent::Ready);
        let stop = StopHandle::default();
        let stop2 = stop.clone();
        let run: SessionRun = Box::pin(async move {
            // Wait for Release to actually land (stop.stop() called) before
            // sending Transcribing — this is the causal ordering that
            // guarantees `state` is already `Finalizing` by the time the
            // controller routes this event (see the test's doc comment).
            while !stop2.is_stopped() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let _ = tx.send(OrchestratorEvent::Transcribing).await;
            let _ = tx.send(OrchestratorEvent::Final("hello".into())).await;
            let _ = tx.send(OrchestratorEvent::Done("hello".into())).await;
            Ok(SessionOutcome::Completed {
                transcript: "hello".into(),
            })
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
            IndicatorState::Finalizing,
            IndicatorState::Hidden,
        ],
        "a Transcribing event arriving after Release must not reopen \
         Recording — got {seq:?} (expected a clean finalizing → hidden, no \
         flicker back to recording)"
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
        states
            .iter()
            .any(|s| matches!(s, IndicatorState::Error { message, .. } if message == "boom")),
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
            let _ = tx
                .send(OrchestratorEvent::Done("first second".into()))
                .await;
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
        two_segment_focus_session(SessionOutcome::Completed {
            transcript: "first second".into(),
        }),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    // "first" was buffered but not yet flushed when focus left; committing it now
    // would land in the *new* surface, so it is discarded — nothing lands after
    // focus loss (SC-007). (With commit-on-finalize the whole burst arrives
    // after finish, so this is the realistic outcome.)
    assert!(log.commits.is_empty(), "nothing committed after focus-out");
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn focus_out_protection_holds_for_every_utterance() {
    // Regression (observed live on Wayland): `focus_events` used to be
    // single-consumer — utterance 1 took the stream, so utterances 2+ selected
    // on an empty one and focus-loss was ignored; a session started in a
    // terminal committed into a password field after a mid-session click
    // (FR-014/FR-022 violation). Every utterance must receive focus events.
    let injector = MockInjector::new().with_focus_events([myna_desktop::FocusEvent::FocusOut]);
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Press],
        injector,
        MockIndicator::new(),
        // Re-usable factory: each utterance buffers "first" and emits the tail
        // + Done only after stop (commit-on-finalize shape).
        two_segment_focus_session(SessionOutcome::Completed {
            transcript: "first second".into(),
        }),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(log.acquires, 2, "both utterances ran");
    assert!(
        log.commits.is_empty(),
        "focus-out must suppress commits in EVERY utterance, got {:?}",
        log.commits
    );
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
        two_segment_focus_session(SessionOutcome::Completed {
            transcript: "first second".into(),
        }),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert!(
        log.commits.is_empty(),
        "nothing committed after target-gone"
    );
    assert!(log.cancels >= 1, "target-gone must cancel");
    assert_eq!(log.restores, 1, "engine restored exactly once");
    assert!(
        matches!(
            indicate_log.lock().unwrap().last(),
            Some(IndicatorState::Error { .. })
        ),
        "target-gone must notify"
    );
    assert_eq!(controller.state(), DictationState::Idle);
}

/// Regression (manual test report, 2026-07-31): a `FocusOut`-terminated
/// utterance with an empty transcript must surface "Focus lost", not "No
/// speech detected" — the session was deliberately cut short, so the empty
/// transcript doesn't mean the user said nothing.
#[tokio::test]
async fn focus_out_with_empty_transcript_surfaces_focus_lost_not_no_speech() {
    let injector = MockInjector::new().with_focus_events([myna_desktop::FocusEvent::FocusOut]);
    let indicator = MockIndicator::new();
    let indicate_log = indicator.log();
    let mut controller = build(
        [TriggerEdge::Press],
        injector,
        indicator,
        // The session must actually block on `stop.stop()` (as the real
        // orchestrator does) rather than complete instantly, so the FocusOut
        // event has a real chance to land before the session's own natural
        // completion — `events_session` completes immediately and would race
        // right past the focus event, silently exercising the wrong path.
        two_segment_focus_session(SessionOutcome::Completed {
            transcript: String::new(),
        }),
    );
    controller.run().await;

    assert_eq!(
        indicate_log.lock().unwrap().last(),
        Some(&IndicatorState::recoverable("Focus lost")),
        "an empty-transcript completion caused by focus-loss must say \
         \"Focus lost\", never \"No speech detected\""
    );
}

// ── Regression: FocusOut must resync the trigger's toggle parity ───────────
// (manual test report, 2026-07-31: "have to press the hotkey twice" to resume
// dictation after the target field lost focus.)

/// A toggle-tracking mock trigger mirroring the *real* `ControlTrigger`'s
/// press/release parity — unlike [`ScriptedTrigger`], which just replays a
/// fixed list of edges with no internal state, this tracks a `pressed` bit
/// that flips on every poke, exactly like the production control-socket /
/// portal-toggle triggers. This is the seam needed to observe the "next poke
/// delivers a swallowed Release instead of Press" desync bug: `ScriptedTrigger`
/// has no parity to desync, so it can never catch this regression.
struct ToggleMockTrigger {
    pokes_remaining: usize,
    pressed: bool,
}

impl ToggleMockTrigger {
    fn new(pokes: usize) -> Self {
        Self {
            pokes_remaining: pokes,
            pressed: false,
        }
    }
}

#[async_trait::async_trait]
impl myna_orchestrator::Trigger for ToggleMockTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        if self.pokes_remaining == 0 {
            return None;
        }
        self.pokes_remaining -= 1;
        self.pressed = !self.pressed;
        Some(if self.pressed {
            TriggerEdge::Press
        } else {
            TriggerEdge::Release
        })
    }

    async fn resync(&mut self) {
        self.pressed = false;
    }
}

#[tokio::test]
async fn focus_out_resyncs_trigger_so_the_very_next_poke_starts_a_new_utterance() {
    // Two physical pokes: the first starts utterance 1 (Press); FocusOut ends
    // it via `stop.stop()` without the controller ever reading a matching
    // edge off the trigger. Without the fix, the trigger's `pressed` bit is
    // left `true` from utterance 1's own Press, so poke 2 flips it to
    // `false` and delivers a `Release` — silently swallowed by the outer
    // idle-wait loop — and the trigger then runs out of scripted pokes
    // (`None`), ending the whole controller after only ONE utterance. With
    // the fix (`trigger.resync()` called on FocusOut), poke 2 correctly
    // delivers a fresh `Press`, and utterance 2 runs.
    let trigger = ToggleMockTrigger::new(2);
    let injector = MockInjector::new().with_focus_events([myna_desktop::FocusEvent::FocusOut]);
    let inject_log = injector.log();
    let mut controller = DesktopController::builder()
        .trigger(trigger)
        .injector(injector)
        .indicator(MockIndicator::new())
        .session(two_segment_focus_session(SessionOutcome::Completed {
            transcript: "first second".into(),
        }))
        .build();
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(
        log.acquires, 2,
        "both utterances must run — the second poke must deliver a real \
         Press, not a stray Release silently swallowed while idle (got {} \
         acquire(s))",
        log.acquires
    );
}

// ── R9 streaming preedit (opt-in): Unstable → preedit, never committed ──────

/// Build a controller with the preedit opt-in switched on (the `build` helper
/// covers the commit-only default).
fn build_preedit(
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
        .preedit(true)
        .build()
}

/// The preedit opt-in follows a setting the user can change while the daemon
/// runs, so it is read per event, not captured at startup. Switching it on
/// mid-utterance has to reach the *next* hypothesis: anything coarser (per
/// press, per process) is a restart wearing a different name.
#[tokio::test]
async fn a_live_preedit_switch_reaches_the_next_hypothesis() {
    let injector = MockInjector::new().with_preedit_support();
    let inject_log = injector.log();
    let preedit = Live::new(false);

    let mut slot = Some(preedit.clone());
    let session = move |tx: mpsc::Sender<OrchestratorEvent>| {
        let switch = slot.take().expect("single-use session");
        let run: SessionRun = Box::pin(async move {
            let _ = tx.send(OrchestratorEvent::Unstable("before".into())).await;
            // A small sleep lets the controller's select poll `events_rx` and
            // route "before" (with preedit=false) before the switch flips.
            // `yield_now` is not sufficient without `biased` because the
            // select may re-poll `run` before `events_rx`.
            tokio::time::sleep(Duration::from_millis(10)).await;
            switch.set(true);
            let _ = tx.send(OrchestratorEvent::Unstable("after".into())).await;
            let _ = tx.send(OrchestratorEvent::Final("after".into())).await;
            let _ = tx.send(OrchestratorEvent::Done("after".into())).await;
            Ok(SessionOutcome::Completed {
                transcript: "after".into(),
            })
        });
        (run, StopHandle::default())
    };

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([
            TriggerEdge::Press,
            TriggerEdge::Release,
        ]))
        .injector(injector)
        .indicator(MockIndicator::new())
        .session(session)
        .preedit(preedit)
        .build();
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(
        log.preedits,
        vec!["after"],
        "the hypothesis before the switch must not be shown, the one after must be"
    );
    assert_eq!(log.commits, vec!["after"], "commits are unaffected");
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn streaming_unstable_is_preedit_only_never_committed() {
    // R9 / FR-012-relaxed-for-preedit: with `--preedit`, each `Unstable`
    // hypothesis is rendered (replaced) in the preedit region; only `Final`
    // text is ever committed. A pending stable burst must be committed *before*
    // the preedit tail that follows it, so the volatile text is drawn after the
    // committed text it extends.
    let injector = MockInjector::new().with_preedit_support();
    let inject_log = injector.log();
    let mut controller = build_preedit(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Unstable("hel".into()),
                OrchestratorEvent::Unstable("hello".into()),
                OrchestratorEvent::Final("hello".into()),
                OrchestratorEvent::Unstable("hello wor".into()),
                OrchestratorEvent::Final("world".into()),
                OrchestratorEvent::Done("hello world".into()),
            ],
            SessionOutcome::Completed {
                transcript: "hello world".into(),
            },
        ),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    // Every unstable hypothesis shown, in order; none committed.
    assert_eq!(log.preedits, vec!["hel", "hello", "hello wor"]);
    // Spaced streaming commits flush separately; the second gets a separator
    // from the text already in the field (stripped-segment server).
    assert_eq!(log.commits, vec!["hello", " world"]);
    // (A preedit string may later appear as a commit — "hello" stabilized and
    // was committed by its `Final`. The invariant is the *channel*: volatile
    // text only ever enters via set_preedit, stable text only via commit.)
    // The commit of "hello" lands before the preedit tail "hello wor" that
    // extends it (the real IBus injector's commit clears the prior preedit).
    assert_eq!(
        log.order,
        vec!["preedit", "preedit", "commit", "preedit", "commit"]
    );
    assert_eq!(controller.state(), DictationState::Idle);
}

#[tokio::test]
async fn stripped_streaming_segments_are_spaced_across_flushes() {
    // Regression (observed live): a whisper streaming server emits committed
    // deltas stripped of whitespace ("This", "is not", "working that
    // well."), and spaced commits flush separately — verbatim insertion
    // produced "Thisis notworking that well.". The controller must separate
    // commits that don't carry their own whitespace.
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Final("This".into()),
                OrchestratorEvent::Unstable("is not".into()),
                OrchestratorEvent::Final("is not".into()),
                OrchestratorEvent::Unstable("working that".into()),
                OrchestratorEvent::Final("working that well.".into()),
                OrchestratorEvent::Done("This is not working that well.".into()),
            ],
            SessionOutcome::Completed {
                transcript: "This is not working that well.".into(),
            },
        ),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert_eq!(
        log.commits.concat(),
        "This is not working that well.",
        "verbatim concatenation of inserted commits must reproduce the transcript: {:?}",
        log.commits
    );
}

#[tokio::test]
async fn natural_spacing_segments_get_no_double_spaces() {
    // Contract I2 servers emit segments with natural (leading-space)
    // whitespace: they concatenate verbatim — the controller must NOT add
    // separators, neither across flushes nor inside a coalesced burst.
    let injector = MockInjector::new();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Final("He began to wish".into()),
                OrchestratorEvent::Unstable(" that he had".into()),
                OrchestratorEvent::Final(" that he had".into()),
                OrchestratorEvent::Final(" compromised in some way.".into()),
                OrchestratorEvent::Done(
                    "He began to wish that he had compromised in some way.".into(),
                ),
            ],
            SessionOutcome::Completed {
                transcript: "He began to wish that he had compromised in some way.".into(),
            },
        ),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    // The two trailing Finals form one burst → coalesced into one commit;
    // the leading spaces are preserved verbatim, never doubled.
    assert_eq!(
        log.commits,
        vec![
            "He began to wish".to_string(),
            " that he had compromised in some way.".to_string()
        ]
    );
    assert_eq!(
        log.commits.concat(),
        "He began to wish that he had compromised in some way."
    );
}

#[tokio::test]
async fn preedit_is_off_by_default_even_when_supported() {
    // FR-012 commit-only is the default: without the opt-in, `Unstable` events
    // are ignored by the injector even when the backend has a preedit region.
    let injector = MockInjector::new().with_preedit_support();
    let inject_log = injector.log();
    let mut controller = build(
        [TriggerEdge::Press, TriggerEdge::Release],
        injector,
        MockIndicator::new(),
        events_session(
            vec![
                OrchestratorEvent::Unstable("hel".into()),
                OrchestratorEvent::Final("hello".into()),
                OrchestratorEvent::Done("hello".into()),
            ],
            SessionOutcome::Completed {
                transcript: "hello".into(),
            },
        ),
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert!(log.preedits.is_empty(), "preedit must be opt-in");
    assert_eq!(log.commits, vec!["hello"]);
}

#[tokio::test]
async fn preedit_suppressed_with_commits_after_focus_loss() {
    // FR-014/SC-007: focus lost mid-session — nothing further lands in the
    // (new) surface, commits AND preedit alike.
    let injector = MockInjector::new()
        .with_preedit_support()
        .with_focus_events([myna_desktop::FocusEvent::FocusOut]);
    let inject_log = injector.log();
    let session = move |tx: mpsc::Sender<OrchestratorEvent>| {
        let _ = tx.try_send(OrchestratorEvent::Final("first".into()));
        let stop = StopHandle::default();
        let stop2 = stop.clone();
        let run: SessionRun = Box::pin(async move {
            while !stop2.is_stopped() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let _ = tx
                .send(OrchestratorEvent::Unstable("first sec".into()))
                .await;
            let _ = tx.send(OrchestratorEvent::Final("second".into())).await;
            let _ = tx
                .send(OrchestratorEvent::Done("first second".into()))
                .await;
            Ok(SessionOutcome::Completed {
                transcript: "first second".into(),
            })
        });
        (run, stop)
    };
    let mut controller = build_preedit(
        [TriggerEdge::Press],
        injector,
        MockIndicator::new(),
        session,
    );
    controller.run().await;

    let log = inject_log.lock().unwrap();
    assert!(log.commits.is_empty(), "nothing committed after focus-out");
    assert!(log.preedits.is_empty(), "no preedit after focus-out");
    assert_eq!(controller.state(), DictationState::Idle);
}
