//! The async driver (plan T40) — the thin async shell around the pure
//! [`Fsm`](crate::fsm::Fsm).
//!
//! All decisions live in the FSM; this module only *pumps*: it opens a backend
//! session, then in a `select!` loop feeds client inputs and backend transcript
//! events into [`Fsm::on_input`], and executes the [`Action`]s it hands back
//! (queue audio up, emit [`OrchestratorEvent`]s out). It runs until the session
//! reaches a terminal state, then returns the [`SessionOutcome`].
//!
//! No arm of the loop waits on another: audio is read only while the FSM's
//! gate is open and the transport has room, so congestion backs up into
//! capture, while control input, backend events and the progress deadline stay
//! live throughout, including during the handshake.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::backend::{BackendClient, BackendError, BackendSink, Outbound};
use crate::fsm::{Action, Fsm, Input, OrchestratorEvent, SessionOutcome};
use myna_core::{PcmChunk, SessionConfig};

/// How long the client waits for any sign of backend progress (a server data
/// frame, event or not, or outbound audio the transport takes) once capture
/// has ended or end-of-audio is queued; each sign restarts it. Nothing arms it
/// while capture is live, where the capture buffer's overload bound ends a
/// stalled session instead. It never limits how long an utterance may be.
pub const BACKEND_PROGRESS_TIMEOUT: Duration = Duration::from_secs(300);

/// Ordered client input: audio, then end-of-audio. Read only while the FSM
/// accepts audio, so it waits (in this channel, then in capture) until then.
#[derive(Debug)]
pub enum OrchestratorInput {
    /// A PCM chunk from capture.
    Audio(PcmChunk),
    /// Hotkey released — end of the current utterance.
    EndOfAudio,
}

/// Out-of-band client input, handled ahead of any queued audio.
#[derive(Debug)]
pub enum OrchestratorControl {
    /// Abandon the utterance.
    Abort,
    /// Local audio capture faulted (device gone, overload, …). Surfaces as a
    /// `Failed` outcome rather than a silent abort.
    CaptureFailed { message: String },
    /// Capture has ended, though its audio may still be queued. From here the
    /// client is only waiting on the backend, so the progress deadline arms.
    CaptureEnded,
}

/// The [`BACKEND_PROGRESS_TIMEOUT`] clock; unarmed until [`Deadline::arm`].
#[derive(Default)]
struct Deadline(Option<Instant>);

impl Deadline {
    fn arm(&mut self) {
        self.0 = Some(Instant::now() + BACKEND_PROGRESS_TIMEOUT);
    }

    fn progress(&mut self) {
        if self.0.is_some() {
            self.arm();
        }
    }

    async fn expired(&self) {
        match self.0 {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    }
}

/// Open a session against `backend` and run the FSM to completion.
///
/// - `inputs` delivers audio and end-of-audio in order; when it closes, the
///   driver keeps draining backend events (so a dropped input source can't
///   strand a `Finalizing` session mid-commit).
/// - `control` delivers abort and capture outcomes ahead of queued audio.
/// - `outputs` receives every [`OrchestratorEvent`]; a closed `outputs` is
///   tolerated — the run still completes and returns its outcome.
///
/// Returns [`BackendError`] only if the *handshake* fails (the session never
/// opened); once open, transport failures surface as a `Failed`
/// [`SessionOutcome`] via the FSM, not as an `Err`.
pub async fn run_session<B: BackendClient>(
    backend: &B,
    config: SessionConfig,
    mut inputs: mpsc::Receiver<OrchestratorInput>,
    mut control: mpsc::Receiver<OrchestratorControl>,
    outputs: mpsc::Sender<OrchestratorEvent>,
) -> Result<SessionOutcome, BackendError> {
    let mut fsm = Fsm::new();
    let mut deadline = Deadline::default();
    let mut control_open = true;
    // The one item waiting for room in the transport.
    let mut outbound: Option<Outbound> = None;

    let opening = backend.open_session(config);
    tokio::pin!(opening);
    let handle = loop {
        let input = tokio::select! {
            biased;
            c = control.recv(), if control_open => control_input(c, &mut control_open, &mut deadline),
            () = deadline.expired() => Some(Input::Stalled),
            opened = &mut opening => break opened?,
        };
        if let Some(input) = input {
            apply(fsm.on_input(input), None, &mut outbound, &outputs).await;
            if let Some(outcome) = fsm.outcome() {
                return Ok(outcome);
            }
        }
    };
    let (sink, mut events, _protocol_version) = handle.split();
    let mut activity = events.activity();
    let mut inputs_open = true;

    while !fsm.state().session.is_terminal() {
        let input = tokio::select! {
            biased;
            c = control.recv(), if control_open => control_input(c, &mut control_open, &mut deadline),
            incoming = events.next() => {
                deadline.progress();
                Some(match incoming {
                    Some(Ok(event)) => Input::Backend(event),
                    Some(Err(err)) => Input::BackendClosed { error: Some(err.to_string()) },
                    None => Input::BackendClosed { error: None },
                })
            }
            () = activity.seen() => {
                deadline.progress();
                None
            }
            permit = sink.reserve(), if outbound.is_some() => {
                // A refused permit means the transport is gone; its closed
                // event stream fails the session.
                if let (Ok(permit), Some(item)) = (permit, outbound.take()) {
                    deadline.progress();
                    permit.send(item);
                }
                None
            }
            client = inputs.recv(), if inputs_open && outbound.is_none() && fsm.accepts_audio() => {
                match client {
                    Some(OrchestratorInput::Audio(chunk)) => Some(Input::Audio(chunk)),
                    Some(OrchestratorInput::EndOfAudio) => {
                        deadline.arm();
                        Some(Input::EndOfAudio)
                    }
                    None => {
                        inputs_open = false;
                        None
                    }
                }
            }
            () = deadline.expired() => Some(Input::Stalled),
        };
        if let Some(input) = input {
            apply(fsm.on_input(input), Some(&sink), &mut outbound, &outputs).await;
        }
    }

    Ok(fsm
        .outcome()
        .expect("loop exits only on a terminal session, which always has an outcome"))
}

fn control_input(
    control: Option<OrchestratorControl>,
    open: &mut bool,
    deadline: &mut Deadline,
) -> Option<Input> {
    match control {
        Some(OrchestratorControl::Abort) => Some(Input::Abort),
        Some(OrchestratorControl::CaptureFailed { message }) => {
            Some(Input::CaptureFailed { message })
        }
        Some(OrchestratorControl::CaptureEnded) => {
            deadline.arm();
            None
        }
        None => {
            *open = false;
            None
        }
    }
}

/// Execute the FSM's actions. Transport items are queued in `outbound` for
/// the loop to hand over when there is room; an abort needs no room.
async fn apply(
    actions: Vec<Action>,
    sink: Option<&BackendSink>,
    outbound: &mut Option<Outbound>,
    outputs: &mpsc::Sender<OrchestratorEvent>,
) {
    for action in actions {
        match action {
            Action::ForwardAudio(chunk) => queue(outbound, Outbound::Audio(chunk)),
            Action::SendFinish => queue(outbound, Outbound::Finish),
            Action::SendAbort => {
                if let Some(sink) = sink {
                    sink.abort();
                }
            }
            Action::Emit(event) => {
                let _ = outputs.send(event).await;
            }
        }
    }
}

/// Transport items come only from client input, which is read only while the
/// slot is empty, so one can never overwrite another. Asserted rather than
/// debug-asserted: an overwrite here drops a PCM chunk or an end of audio,
/// which a release build must not do silently.
fn queue(outbound: &mut Option<Outbound>, item: Outbound) {
    assert!(outbound.is_none(), "{item:?} would overwrite {outbound:?}");
    *outbound = Some(item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::backend::{channels, BackendHandle, EventSender, Outbox};
    use myna_core::{AudioFormat, PcmChunk, Progress, TranscriptionEvent, PHASE_READY};

    fn chunk() -> PcmChunk {
        PcmChunk::new(vec![0u8; 3200], AudioFormat::default())
    }

    /// Drain every output event the run produced (the outputs channel is closed
    /// by dropping the sender once the run returns).
    async fn collect(mut rx: mpsc::Receiver<OrchestratorEvent>) -> Vec<OrchestratorEvent> {
        let mut events = Vec::new();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        events
    }

    /// The slot holds exactly one item: overwriting it would drop audio or an
    /// end of audio, so it is a panic in every build, not just a debug one.
    #[test]
    #[should_panic(expected = "would overwrite")]
    fn queueing_over_a_full_outbound_slot_panics() {
        let mut outbound = Some(Outbound::Finish);
        queue(&mut outbound, Outbound::Audio(chunk()));
    }

    #[tokio::test]
    async fn happy_path_full_status_sequence() {
        let backend = FakeBackend::happy_path();
        let (in_tx, in_rx) = mpsc::channel(16);
        let (_control_tx, control_rx) = mpsc::channel(4);
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(
                &backend,
                SessionConfig::default(),
                in_rx,
                control_rx,
                out_tx,
            )
            .await
        });

        // Speak a little, then release the hotkey.
        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        in_tx.send(OrchestratorInput::EndOfAudio).await.unwrap();
        drop(in_tx);

        let outcome = driver.await.unwrap().expect("session opened");
        let events = collect(out_rx).await;

        // The residency liveness sequence is observed in order (transcribing
        // repeats as a liveness ping per progress event — we assert the
        // first-occurrence ordering, not the count).
        let mut milestones: Vec<&OrchestratorEvent> = Vec::new();
        for e in &events {
            if matches!(
                e,
                OrchestratorEvent::Loading
                    | OrchestratorEvent::Ready
                    | OrchestratorEvent::Transcribing
            ) && !milestones.contains(&e)
            {
                milestones.push(e);
            }
        }
        assert_eq!(
            milestones,
            vec![
                &OrchestratorEvent::Loading,
                &OrchestratorEvent::Ready,
                &OrchestratorEvent::Transcribing,
            ],
            "STATUS loading→ready→transcribing exercised over the wire",
        );

        // ...and the transcript comes through as finals + a terminal done.
        let finals: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                OrchestratorEvent::Final(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            finals,
            vec!["The quick brown fox", "jumps over the lazy dog."]
        );
        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "The quick brown fox jumps over the lazy dog.".into()
            }
        );
    }

    #[tokio::test]
    async fn commit_drain_tail_arrives_after_finish() {
        // §3C over the wire: the fake backend holds the tail final + done until
        // it receives `session.finish`.
        let backend = FakeBackend::commit_drain();
        let (in_tx, in_rx) = mpsc::channel(16);
        let (_control_tx, control_rx) = mpsc::channel(4);
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(
                &backend,
                SessionConfig::default(),
                in_rx,
                control_rx,
                out_tx,
            )
            .await
        });

        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        in_tx.send(OrchestratorInput::EndOfAudio).await.unwrap();
        drop(in_tx);

        let outcome = driver.await.unwrap().expect("session opened");
        let events = collect(out_rx).await;

        let finals: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                OrchestratorEvent::Final(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            finals,
            vec!["the quick brown fox", "jumps over the lazy dog."]
        );
        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "the quick brown fox jumps over the lazy dog.".into()
            }
        );
    }

    #[tokio::test]
    async fn mid_stream_error_fails_the_session() {
        // §3B: a terminal error mid-stream ends the run as Failed.
        let backend = FakeBackend::mid_stream_error("inference_failed", "decode blew up");
        let (in_tx, in_rx) = mpsc::channel(16);
        let (_control_tx, control_rx) = mpsc::channel(4);
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(
                &backend,
                SessionConfig::default(),
                in_rx,
                control_rx,
                out_tx,
            )
            .await
        });

        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        drop(in_tx);

        let outcome = driver.await.unwrap().expect("session opened");
        let events = collect(out_rx).await;

        assert!(events.iter().any(|e| matches!(
            e,
            OrchestratorEvent::Error { code, .. } if code == "inference_failed"
        )));
        assert_eq!(
            outcome,
            SessionOutcome::Failed {
                code: "inference_failed".into(),
                message: "decode blew up".into()
            }
        );
    }

    #[tokio::test]
    async fn abort_before_finish_commits_nothing() {
        let backend = FakeBackend::commit_drain();
        let (in_tx, in_rx) = mpsc::channel(16);
        let (control_tx, control_rx) = mpsc::channel(4);
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(
                &backend,
                SessionConfig::default(),
                in_rx,
                control_rx,
                out_tx,
            )
            .await
        });

        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        control_tx.send(OrchestratorControl::Abort).await.unwrap();

        let outcome = driver.await.unwrap().expect("session opened");
        let _ = collect(out_rx).await;
        assert_eq!(outcome, SessionOutcome::Aborted);
    }

    /// The backend end of a probe session, which never reads audio unless
    /// told to.
    struct End {
        out: Outbox,
        events: EventSender,
    }

    struct Probe(mpsc::UnboundedSender<End>);

    #[async_trait::async_trait]
    impl BackendClient for Probe {
        async fn open_session(
            &self,
            _config: SessionConfig,
        ) -> Result<crate::backend::BackendHandle, BackendError> {
            let (sink, out, events, ev_tx) = channels(16, 64);
            let _ = self.0.send(End { out, events: ev_tx });
            Ok(BackendHandle::new(sink, events, None))
        }
    }

    /// A backend whose handshake never completes.
    struct NeverOpens;

    #[async_trait::async_trait]
    impl BackendClient for NeverOpens {
        async fn open_session(
            &self,
            _config: SessionConfig,
        ) -> Result<crate::backend::BackendHandle, BackendError> {
            std::future::pending().await
        }
    }

    struct Session {
        audio: mpsc::Sender<OrchestratorInput>,
        control: mpsc::Sender<OrchestratorControl>,
        run: tokio::task::JoinHandle<Result<SessionOutcome, BackendError>>,
    }

    fn start<B: BackendClient + 'static>(backend: B) -> Session {
        let (audio, in_rx) = mpsc::channel(16);
        let (control, control_rx) = mpsc::channel(4);
        let (out_tx, mut out_rx) = mpsc::channel(64);
        tokio::spawn(async move { while out_rx.recv().await.is_some() {} });
        let run = tokio::spawn(async move {
            run_session(
                &backend,
                SessionConfig::default(),
                in_rx,
                control_rx,
                out_tx,
            )
            .await
        });
        Session {
            audio,
            control,
            run,
        }
    }

    /// A ready probe session whose outbound audio is backed up all the way
    /// into the input channel.
    async fn congested() -> (Session, End) {
        let (tx, mut ends) = mpsc::unbounded_channel();
        let session = start(Probe(tx));
        let end = ends.recv().await.unwrap();
        end.events
            .send(Ok(TranscriptionEvent::Progress(Progress::phase(
                PHASE_READY,
            ))))
            .await
            .unwrap();
        let audio = session.audio.clone();
        tokio::spawn(async move {
            while audio.send(OrchestratorInput::Audio(chunk())).await.is_ok() {}
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(session.audio.capacity(), 0, "the input channel is full");
        (session, end)
    }

    #[tokio::test(start_paused = true)]
    async fn abort_is_delivered_while_outbound_audio_is_congested() {
        let (session, mut end) = congested().await;
        session
            .control
            .send(OrchestratorControl::Abort)
            .await
            .unwrap();
        let outcome = session.run.await.unwrap().unwrap();
        assert_eq!(outcome, SessionOutcome::Aborted);
        end.out.abort.aborted().await;
    }

    #[tokio::test(start_paused = true)]
    async fn capture_failure_is_handled_while_outbound_audio_is_congested() {
        let (session, mut end) = congested().await;
        let message = "microphone unplugged".to_string();
        session
            .control
            .send(OrchestratorControl::CaptureFailed { message })
            .await
            .unwrap();
        let outcome = session.run.await.unwrap().unwrap();
        assert!(
            matches!(outcome, SessionOutcome::Failed { ref code, .. } if code == "capture_failed")
        );
        end.out.abort.aborted().await;
    }

    #[tokio::test(start_paused = true)]
    async fn audio_waits_unread_until_the_backend_is_ready() {
        let (tx, mut ends) = mpsc::unbounded_channel();
        let session = start(Probe(tx));
        let mut end = ends.recv().await.unwrap();
        session
            .audio
            .send(OrchestratorInput::Audio(chunk()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(session.audio.capacity(), 15, "audio was read before ready");

        end.events
            .send(Ok(TranscriptionEvent::Progress(Progress::phase(
                PHASE_READY,
            ))))
            .await
            .unwrap();
        assert!(matches!(
            end.out.queue.recv().await,
            Some(Outbound::Audio(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_capture_fault_during_the_handshake_fails_without_waiting_for_it() {
        let session = start(NeverOpens);
        let message = "no microphone".to_string();
        session
            .control
            .send(OrchestratorControl::CaptureFailed { message })
            .await
            .unwrap();
        let outcome = session.run.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            SessionOutcome::Failed {
                code: "capture_failed".into(),
                message: "no microphone".into()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn abort_during_the_handshake_ends_the_session() {
        let session = start(NeverOpens);
        session
            .control
            .send(OrchestratorControl::Abort)
            .await
            .unwrap();
        assert_eq!(session.run.await.unwrap().unwrap(), SessionOutcome::Aborted);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_handshake_fails_once_capture_has_ended() {
        let session = start(NeverOpens);
        let started = Instant::now();
        session
            .control
            .send(OrchestratorControl::CaptureEnded)
            .await
            .unwrap();
        let outcome = session.run.await.unwrap().unwrap();
        assert!(
            matches!(outcome, SessionOutcome::Failed { ref code, .. } if code == "backend_unresponsive")
        );
        assert_eq!(started.elapsed().as_secs(), 300);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_control_channel_leaves_the_session_running() {
        let backend = FakeBackend::commit_drain();
        let session = start(backend);
        drop(session.control);
        session
            .audio
            .send(OrchestratorInput::Audio(chunk()))
            .await
            .unwrap();
        session
            .audio
            .send(OrchestratorInput::EndOfAudio)
            .await
            .unwrap();
        assert!(matches!(
            session.run.await.unwrap().unwrap(),
            SessionOutcome::Completed { .. }
        ));
    }
}
