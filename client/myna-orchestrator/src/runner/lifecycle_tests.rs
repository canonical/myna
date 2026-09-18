//! Runner lifecycle on tokio's paused clock: capture faults and releases
//! before readiness, readiness lapses mid-utterance, and the backend progress
//! deadline. Every run is bounded by an hour of virtual time, so a hang fails
//! instead of stalling the suite.

use std::future::Future;
use std::time::Duration;

use myna_audio::{CaptureSource, ScriptedBackend, Step};
use myna_core::{
    AudioFormat, Disposition, ErrorData, Progress, SessionConfig, TranscriptionEvent,
    TranscriptionFinal, PHASE_PREPARING, PHASE_READY, PROTOCOL_VERSION,
};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::run_dictation;
use crate::audio::{AudioSource, WavFileSource};
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

    /// A committed segment: the text an injector would insert.
    async fn committed(&self, text: &str) {
        self.emit(TranscriptionEvent::Final(TranscriptionFinal {
            text: text.into(),
            disposition: Disposition::Committed,
            ..Default::default()
        }))
        .await;
    }

    async fn failed(&self, code: &str, message: &str) {
        self.emit(TranscriptionEvent::Error(ErrorData {
            code: code.into(),
            message: message.into(),
        }))
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
    dictate_from(mic(steps), serve).await
}

/// [`dictate`] over a source the test built itself (a narrower ring, a clip).
async fn dictate_from<A, S, F, T>(source: A, serve: S) -> (Run, End, T)
where
    A: AudioSource + 'static,
    S: FnOnce(End) -> F,
    F: Future<Output = (End, T)>,
{
    let (tx, mut ends) = mpsc::unbounded_channel();
    let backend = Probe(tx);
    let mut sink = CollectingSink::default();
    let start = Instant::now();
    let run = async {
        let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink).await;
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

/// Where `what` landed in the event stream, so a test can assert the order the
/// user experiences (text, then the fault).
fn at(events: &[OrchestratorEvent], what: impl Fn(&OrchestratorEvent) -> bool) -> usize {
    events
        .iter()
        .position(what)
        .unwrap_or_else(|| panic!("the event never arrived: {events:?}"))
}

fn is_done(event: &OrchestratorEvent) -> bool {
    matches!(event, OrchestratorEvent::Done(_))
}

fn is_error(event: &OrchestratorEvent) -> bool {
    matches!(event, OrchestratorEvent::Error { .. })
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
async fn a_capture_fault_transcribes_the_audio_already_captured_before_reporting_it() {
    // The microphone dies mid-utterance: what was already captured is still
    // sent, in order, finished and transcribed, and the device failure is the
    // last thing the user hears about.
    let steps = vec![
        Step::Bytes(vec![1; 3200]),
        Step::Bytes(vec![2; 3200]),
        Step::Bytes(vec![3; 3200]),
        Step::Fault("the microphone was unplugged".into()),
    ];
    let (run, _end, received) = dictate(steps, |mut end| async {
        end.ready().await;
        let received = end.read_to_finish().await;
        end.committed("what I had already said").await;
        end.done("what I had already said").await;
        (end, received)
    })
    .await;

    let expected: Vec<u8> = (1..=3u8).flat_map(|n| vec![n; 3200]).collect();
    assert!(
        received == expected,
        "the audio captured before the fault never reached the backend in order"
    );
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "capture_failed");
    assert!(
        message.contains("the microphone was unplugged"),
        "{message}"
    );
    assert!(message.contains("audio was lost"), "{message}");

    let lost = at(&run.events, |e| {
        matches!(e, OrchestratorEvent::CaptureLost { .. })
    });
    let text = at(
        &run.events,
        |e| matches!(e, OrchestratorEvent::Final(t) if t == "what I had already said"),
    );
    let done = at(&run.events, is_done);
    let error = at(&run.events, is_error);
    assert!(
        lost < done && text < done && done < error,
        "the fault must come after the transcript: {:?}",
        run.events
    );
    assert!(run.elapsed < Duration::from_secs(1), "{:?}", run.elapsed);
}

#[tokio::test(start_paused = true)]
async fn a_capture_fault_before_any_audio_fails_at_once_with_no_transcript() {
    let (run, mut end, ()) = dictate(vec![Step::Fault("no microphone".into())], |end| async {
        end.ready().await;
        (end, ())
    })
    .await;
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "capture_failed");
    assert!(message.contains("no microphone"), "{message}");
    assert!(
        !message.contains("audio was lost"),
        "there was no audio to lose: {message}"
    );
    assert!(run.elapsed < Duration::from_secs(1), "{:?}", run.elapsed);
    assert!(
        end.out.queue.try_recv().is_err(),
        "nothing to salvage, so nothing is sent"
    );
    assert!(
        !run.events.iter().any(is_done),
        "{:?} claims a transcript",
        run.events
    );
    assert!(
        !run.events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::CaptureLost { .. })),
        "nothing was captured, so nothing is being finished: {:?}",
        run.events
    );
}

#[tokio::test(start_paused = true)]
async fn a_clip_whose_health_never_ends_still_arms_the_progress_deadline() {
    // `WavFileSource` reports `Capturing` and nothing else, so the end of the
    // clip is the only sign capture is over: the end-of-audio path has to
    // arm the deadline for it, or a backend that never loads waits forever.
    let path = super::tests::wav_file(1);
    let source = WavFileSource::new(&path).unwrap().with_chunk_seconds(0.1);
    let (run, _end, ()) = dictate_from(source, |end| async {
        end.loading().await;
        (end, ())
    })
    .await;
    assert_eq!(failure(&run.outcome).0, "backend_unresponsive");
    assert_eq!(run.elapsed.as_secs(), 300);
    std::fs::remove_file(&path).ok();
}

#[tokio::test(start_paused = true)]
async fn an_overloaded_buffer_still_transcribes_the_audio_that_survived() {
    // A one-second buffer with three seconds pushed into it in one go: the
    // first second is retained and the rest is genuinely lost. The words that
    // survived are still transcribed, and the error says audio was lost.
    let source = CaptureSource::builder(AudioFormat::default())
        .ring_depth(Duration::from_secs(1))
        .backend(Box::new(ScriptedBackend::new(vec![silence(3)])))
        .build();
    let (run, _end, received) = dictate_from(source, |mut end| async {
        end.ready().await;
        let received = end.read_to_finish().await;
        end.committed("what survived").await;
        end.done("what survived").await;
        (end, received)
    })
    .await;

    assert_eq!(
        received.len(),
        32_000,
        "the second of audio the buffer held was not sent"
    );
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "capture_failed");
    assert!(message.contains("audio was lost"), "{message}");
    assert!(
        at(&run.events, is_done) < at(&run.events, is_error),
        "{:?}",
        run.events
    );
}

#[tokio::test(start_paused = true)]
async fn a_backend_that_fails_while_finishing_reports_both_failures() {
    let steps = vec![
        Step::Bytes(vec![7; 3200]),
        Step::Fault("device vanished".into()),
    ];
    let (run, _end, ()) = dictate(steps, |mut end| async {
        end.ready().await;
        end.read_to_finish().await;
        end.failed("inference_failed", "decode blew up").await;
        (end, ())
    })
    .await;
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "inference_failed");
    assert!(message.contains("decode blew up"), "{message}");
    assert!(
        message.contains("device vanished"),
        "the device fault was swallowed: {message}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_capture_fault_the_backend_never_finishes_ends_at_the_progress_deadline() {
    // The backend takes the salvaged audio but never answers: the deadline
    // armed by the fault ends the wait, and still reports the device.
    let steps = vec![silence(60), Step::Fault("device vanished".into())];
    let (run, _end, ()) = dictate(steps, |end| async {
        end.ready().await;
        (end, ())
    })
    .await;
    let (code, message) = failure(&run.outcome);
    assert_eq!(code, "backend_unresponsive");
    assert!(message.contains("device vanished"), "{message}");
    assert_eq!(
        run.elapsed.as_secs(),
        300,
        "the deadline arms when capture faults, not when it drains"
    );
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
            .any(|e| matches!(e, OrchestratorEvent::AudioDropped)),
        "{:?}",
        run.events
    );
}

#[tokio::test(start_paused = true)]
async fn capture_is_stopped_by_the_time_the_runner_returns() {
    let (tx, mut ends) = mpsc::unbounded_channel();
    let backend = Probe(tx);
    let source = mic(vec![silence(1), Step::Wait(Duration::from_secs(600))]);
    let stop = source.stop_handle();
    let mut sink = CollectingSink::default();
    let serve = async {
        let end = ends.recv().await.expect("a session opens");
        end.ready().await;
        end.emit(TranscriptionEvent::Error(myna_core::ErrorData {
            code: "inference_failed".into(),
            message: "boom".into(),
        }))
        .await;
        end
    };
    let (outcome, _end) = futures_util::future::join(
        run_dictation(&backend, SessionConfig::default(), source, &mut sink),
        serve,
    )
    .await;
    assert_eq!(failure(&outcome).0, "inference_failed");
    assert!(stop.is_stopped(), "capture outlived the runner");
}

/// A source that reports its fault on one side only. A real device reports it
/// on both - health, and the stream's `Err` after the audio it had queued -
/// but the salvage must not need it twice, nor wait for the side that stays
/// quiet.
struct OneSided {
    health_fault: Option<myna_core::CaptureError>,
    stream_fault: Option<myna_core::CaptureError>,
}

impl AudioSource for OneSided {
    fn format(&self) -> AudioFormat {
        AudioFormat::default()
    }

    fn health(&self) -> myna_core::CaptureHealthStream {
        let mut states = vec![myna_core::CaptureHealth::Capturing];
        // A faulted source never reports `Ended`; one that speaks only
        // through its stream simply stops reporting.
        if let Some(err) = self.health_fault.clone() {
            states.push(myna_core::CaptureHealth::Faulted(err));
        }
        Box::pin(futures_util::stream::iter(states))
    }

    fn capture(self: Box<Self>) -> crate::audio::CaptureStream {
        let chunk = myna_core::PcmChunk::new(vec![9u8; 3200], AudioFormat::default());
        let mut items = vec![Ok(chunk)];
        if let Some(err) = self.stream_fault {
            items.push(Err(err));
        }
        Box::pin(futures_util::stream::iter(items))
    }
}

#[tokio::test(start_paused = true)]
async fn a_fault_reported_on_one_side_only_is_still_salvaged() {
    for (which, source) in [
        (
            "health only",
            OneSided {
                health_fault: Some(myna_core::CaptureError::DeviceUnavailable("gone".into())),
                stream_fault: None,
            },
        ),
        (
            "stream only",
            OneSided {
                health_fault: None,
                stream_fault: Some(myna_core::CaptureError::DeviceUnavailable("gone".into())),
            },
        ),
    ] {
        let (run, _end, received) = dictate_from(source, |mut end| async {
            end.ready().await;
            let received = end.read_to_finish().await;
            end.done("what I had already said").await;
            (end, received)
        })
        .await;
        assert_eq!(received.len(), 3200, "{which}: the chunk never arrived");
        let (code, message) = failure(&run.outcome);
        assert_eq!(code, "capture_failed", "{which}");
        assert!(message.contains("gone"), "{which}: {message}");
        assert!(message.contains("audio was lost"), "{which}: {message}");
        assert!(
            at(&run.events, is_done) < at(&run.events, is_error),
            "{which}: {:?}",
            run.events
        );
        assert!(run.elapsed < Duration::from_secs(1), "{which}: hung");
    }
}
