//! Runner lifecycle on tokio's paused clock: capture faults and releases
//! before readiness, readiness lapses mid-utterance, and the backend progress
//! deadline. Every run is bounded by an hour of virtual time, so a hang fails
//! instead of stalling the suite.

use std::future::Future;
use std::time::Duration;

use myna_audio::{CaptureSource, ScriptedBackend, Step};
use myna_core::{
    AudioFormat, Progress, SessionConfig, TranscriptionEvent, TranscriptionFinal, PHASE_PREPARING,
    PHASE_READY, PROTOCOL_VERSION,
};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::run_dictation;
use crate::backend::{
    channels, BackendClient, BackendError, BackendHandle, EventSender, Outbound, Outbox,
};
use crate::fsm::{OrchestratorEvent, SessionOutcome};
use crate::sink::CollectingSink;

/// The backend end of one session, scripted by the test.
struct End {
    out: Outbox,
    events: EventSender,
}

impl End {
    async fn emit(&self, event: TranscriptionEvent) {
        let _ = self.events.send(Ok(event)).await;
    }

    async fn loading(&self) {
        self.emit(TranscriptionEvent::Progress(Progress::phase(
            PHASE_PREPARING,
        )))
        .await;
    }

    async fn ready(&self) {
        self.emit(TranscriptionEvent::Progress(Progress::phase(PHASE_READY)))
            .await;
    }

    async fn done(&self, text: &str) {
        self.emit(TranscriptionEvent::Done(TranscriptionFinal {
            text: text.into(),
            ..Default::default()
        }))
        .await;
    }

    async fn next_audio(&mut self) -> Vec<u8> {
        match self.out.queue.recv().await {
            Some(Outbound::Audio(chunk)) => chunk.data.to_vec(),
            other => panic!("expected audio, got {other:?}"),
        }
    }

    /// Every audio byte up to the finish, which must come exactly once.
    async fn read_to_finish(&mut self) -> Vec<u8> {
        let mut audio = Vec::new();
        loop {
            match self.out.queue.recv().await {
                Some(Outbound::Audio(chunk)) => audio.extend_from_slice(&chunk.data),
                Some(Outbound::Finish) => return audio,
                None => panic!("the session ended before its finish"),
            }
        }
    }
}

/// Hands each opened session's backend end to the test.
struct Probe(mpsc::UnboundedSender<End>);

#[async_trait::async_trait]
impl BackendClient for Probe {
    async fn open_session(&self, _config: SessionConfig) -> Result<BackendHandle, BackendError> {
        let (sink, out, events, ev_tx) = channels(16, 64);
        let _ = self.0.send(End { out, events: ev_tx });
        Ok(BackendHandle::new(
            sink,
            events,
            Some(PROTOCOL_VERSION.to_string()),
        ))
    }
}

fn mic(steps: Vec<Step>) -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(ScriptedBackend::new(steps)))
        .build()
}

struct Run {
    outcome: Result<SessionOutcome, BackendError>,
    events: Vec<OrchestratorEvent>,
    /// Virtual time from the press to the outcome.
    elapsed: Duration,
}

/// Dictate `steps` into a backend scripted by `serve`, which gets the
/// session's backend end and returns it (keeping the session open) with
/// whatever it observed.
async fn dictate<S, F, T>(steps: Vec<Step>, serve: S) -> (Run, End, T)
where
    S: FnOnce(End) -> F,
    F: Future<Output = (End, T)>,
{
    let (tx, mut ends) = mpsc::unbounded_channel();
    let backend = Probe(tx);
    let mut sink = CollectingSink::default();
    let start = Instant::now();
    let run = async {
        let outcome =
            run_dictation(&backend, SessionConfig::default(), mic(steps), &mut sink).await;
        (outcome, start.elapsed())
    };
    let serve = async { serve(ends.recv().await.expect("a session opens")).await };
    let ((outcome, elapsed), (end, observed)) = tokio::time::timeout(
        Duration::from_secs(3600),
        futures_util::future::join(run, serve),
    )
    .await
    .expect("the utterance hung");
    let run = Run {
        outcome,
        events: sink.events,
        elapsed,
    };
    (run, end, observed)
}

fn failure(outcome: &Result<SessionOutcome, BackendError>) -> (&str, &str) {
    match outcome {
        Ok(SessionOutcome::Failed { code, message }) => (code, message),
        other => panic!("expected a failed session, got {other:?}"),
    }
}

fn silence(seconds: u64) -> Step {
    Step::Silence(Duration::from_secs(seconds))
}

#[tokio::test(start_paused = true)]
async fn a_backend_that_never_becomes_ready_fails_once_capture_has_ended() {
    let (run, _end, ()) = dictate(vec![silence(1)], |end| async {
        end.loading().await;
        (end, ())
    })
    .await;
    assert_eq!(failure(&run.outcome).0, "backend_unresponsive");
    assert_eq!(run.elapsed.as_secs(), 300);
}

#[tokio::test(start_paused = true)]
async fn live_capture_never_arms_the_progress_deadline() {
    let steps = vec![Step::Wait(Duration::from_secs(1000))];
    let (run, _end, ()) = dictate(steps, |end| async {
        end.loading().await;
        (end, ())
    })
    .await;
    assert_eq!(failure(&run.outcome).0, "backend_unresponsive");
    assert_eq!(run.elapsed.as_secs(), 1300);
}

#[tokio::test(start_paused = true)]
async fn a_backend_stalled_while_finalizing_fails_after_the_deadline() {
    let (run, _end, finished_at) = dictate(vec![silence(1)], |mut end| async {
        end.ready().await;
        end.read_to_finish().await;
        let finished_at = Instant::now();
        (end, finished_at)
    })
    .await;
    assert_eq!(failure(&run.outcome).0, "backend_unresponsive");
    let stalled = Instant::now().duration_since(finished_at);
    assert_eq!(stalled.as_secs(), 300);
}

#[tokio::test(start_paused = true)]
async fn server_messages_reset_the_progress_deadline() {
    let (run, _end, ()) = dictate(vec![silence(1)], |mut end| async {
        end.loading().await;
        tokio::time::sleep(Duration::from_secs(250)).await;
        end.loading().await;
        tokio::time::sleep(Duration::from_secs(250)).await;
        end.ready().await;
        end.read_to_finish().await;
        end.done("ok").await;
        (end, ())
    })
    .await;
    assert!(
        matches!(run.outcome, Ok(SessionOutcome::Completed { .. })),
        "{:?}",
        run.outcome
    );
    assert_eq!(run.elapsed.as_secs(), 500);
}

#[tokio::test(start_paused = true)]
async fn audio_the_backend_keeps_accepting_resets_the_progress_deadline() {
    // Ten seconds of audio read one chunk every ten seconds: well past the
    // deadline in total, but never 300 s without the backend taking audio.
    let (run, _end, received) = dictate(vec![silence(10)], |mut end| async {
        end.ready().await;
        let mut received = 0;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            received += end.next_audio().await.len();
        }
        end.read_to_finish().await;
        end.done("ok").await;
        (end, received)
    })
    .await;
    assert!(
        matches!(run.outcome, Ok(SessionOutcome::Completed { .. })),
        "{:?}",
        run.outcome
    );
    assert_eq!(received, 320_000);
}

#[tokio::test(start_paused = true)]
async fn a_backend_that_stops_reading_audio_fails_after_the_deadline() {
    let (run, _end, ()) = dictate(vec![silence(60)], |end| async {
        end.ready().await;
        (end, ())
    })
    .await;
    assert_eq!(failure(&run.outcome).0, "backend_unresponsive");
    assert_eq!(run.elapsed.as_secs(), 300);
}

#[tokio::test(start_paused = true)]
async fn a_capture_fault_is_seen_while_the_backend_is_not_reading() {
    let steps = vec![silence(60), Step::Fault("device vanished".into())];
    let (run, _end, ()) = dictate(steps, |end| async {
        end.ready().await;
        (end, ())
    })
    .await;
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "capture_failed");
    assert!(message.contains("device vanished"), "{message}");
    assert!(run.elapsed < Duration::from_secs(1), "{:?}", run.elapsed);
}

#[tokio::test(start_paused = true)]
async fn release_before_ready_sends_the_prefix_in_order_then_finishes() {
    let steps = (1..=20u8).map(|n| Step::Bytes(vec![n; 3200])).collect();
    let (run, _end, received) = dictate(steps, |mut end| async {
        end.loading().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        end.ready().await;
        let received = end.read_to_finish().await;
        end.done("ok").await;
        (end, received)
    })
    .await;
    assert!(matches!(run.outcome, Ok(SessionOutcome::Completed { .. })));
    let expected: Vec<u8> = (1..=20u8).flat_map(|n| vec![n; 3200]).collect();
    assert!(received == expected, "the prefix arrived out of order");
}

#[tokio::test(start_paused = true)]
async fn loading_after_ready_keeps_forwarding_audio() {
    let steps = vec![
        Step::Bytes(vec![1; 3200]),
        Step::Wait(Duration::from_secs(1)),
        Step::Bytes(vec![2; 3200]),
        Step::Wait(Duration::from_secs(1)),
        Step::Bytes(vec![3; 3200]),
    ];
    let (run, _end, received) = dictate(steps, |mut end| async {
        end.ready().await;
        let mut received = end.next_audio().await;
        end.loading().await;
        received.extend(end.read_to_finish().await);
        end.done("ok").await;
        (end, received)
    })
    .await;
    assert!(matches!(run.outcome, Ok(SessionOutcome::Completed { .. })));
    let expected: Vec<u8> = (1..=3u8).flat_map(|n| vec![n; 3200]).collect();
    assert!(received == expected, "audio after the lapse was lost");
    assert!(
        !run.events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::AudioDropped(_))),
        "{:?}",
        run.events
    );
}
