//! The async driver (plan T40) — the thin async shell around the pure
//! [`Fsm`](crate::fsm::Fsm).
//!
//! All decisions live in the FSM; this module only *pumps*: it opens a backend
//! session, then in a `select!` loop feeds two input sources — client inputs
//! (audio / hotkey, [`OrchestratorInput`]) and backend transcript events — into
//! [`Fsm::on_input`], and executes the [`Action`]s it hands back (forward audio
//! up, emit [`OrchestratorEvent`]s out). It runs until the session reaches a
//! terminal state, then returns the [`SessionOutcome`].
//!
//! Keeping the driver logic-free is the whole point: every interesting behaviour
//! (the accept-gate, commit-drain, pre-ready drop, error mapping) is tested
//! deterministically in [`crate::fsm`]'s unit tests, and the async integration
//! tests here only confirm the wiring holds over real channels against the
//! [`FakeBackend`](crate::backend::fake::FakeBackend).

use tokio::sync::mpsc;

use crate::backend::{BackendClient, BackendError};
use crate::fsm::{Action, Fsm, Input, OrchestratorEvent, SessionOutcome};
use myna_core::{PcmChunk, SessionConfig};

/// What the client feeds *in* — the audio-adapter + hotkey side of the FSM
/// (mocked by stdin/WAV in T41). The backend side is read directly from the
/// [`crate::backend::BackendEvents`] stream, so it is not part of this enum.
#[derive(Debug)]
pub enum OrchestratorInput {
    /// A PCM chunk from capture.
    Audio(PcmChunk),
    /// Hotkey released — end of the current utterance.
    EndOfAudio,
    /// Abandon the utterance.
    Abort,
    /// Local audio capture faulted (device gone, spawn failure, …). Surfaces
    /// as a `Failed` outcome rather than a silent abort.
    CaptureFailed { message: String },
}

/// Open a session against `backend` and run the FSM to completion.
///
/// - `inputs` delivers client-side audio/control; when it closes, the driver
///   stops accepting client input but keeps draining backend events (so a
///   dropped input source can't strand a `Finalizing` session mid-commit).
/// - `outputs` receives every [`OrchestratorEvent`]; the caller (T41's text
///   injector / UI) consumes them. A closed `outputs` is tolerated — the run
///   still completes and returns its outcome.
///
/// Returns [`BackendError`] only if the *handshake* fails (the session never
/// opened); once open, transport failures surface as a `Failed`
/// [`SessionOutcome`] via the FSM, not as an `Err`.
pub async fn run_session<B: BackendClient>(
    backend: &B,
    config: SessionConfig,
    mut inputs: mpsc::Receiver<OrchestratorInput>,
    outputs: mpsc::Sender<OrchestratorEvent>,
) -> Result<SessionOutcome, BackendError> {
    let handle = backend.open_session(config).await?;
    let (sink, mut events, _protocol_version) = handle.split();

    let mut fsm = Fsm::new();
    let mut inputs_open = true;

    while !fsm.state().session.is_terminal() {
        let actions = tokio::select! {
            // Backend events (down): transcript + liveness + terminal.
            incoming = events.next() => match incoming {
                Some(Ok(event)) => fsm.on_input(Input::Backend(event)),
                Some(Err(err)) => fsm.on_input(Input::BackendClosed { error: Some(err.to_string()) }),
                None => fsm.on_input(Input::BackendClosed { error: None }),
            },
            // Client inputs (up): audio + hotkey. Disabled once the source ends.
            client = inputs.recv(), if inputs_open => match client {
                Some(OrchestratorInput::Audio(chunk)) => fsm.on_input(Input::Audio(chunk)),
                Some(OrchestratorInput::EndOfAudio) => fsm.on_input(Input::EndOfAudio),
                Some(OrchestratorInput::Abort) => fsm.on_input(Input::Abort),
                Some(OrchestratorInput::CaptureFailed { message }) => {
                    fsm.on_input(Input::CaptureFailed { message })
                }
                None => {
                    // Source exhausted: stop selecting on it, keep draining the
                    // backend to its terminal event.
                    inputs_open = false;
                    Vec::new()
                }
            },
        };

        for action in actions {
            match action {
                Action::ForwardAudio(chunk) => {
                    // A send failure means the backend task is gone; the next
                    // `events.next()` will yield the close and the FSM will fail.
                    let _ = sink.send_audio(chunk).await;
                }
                Action::SendFinish => {
                    let _ = sink.finish().await;
                }
                Action::SendAbort => {
                    let _ = sink.abort().await;
                }
                Action::Emit(event) => {
                    let _ = outputs.send(event).await;
                }
            }
        }
    }

    Ok(fsm
        .outcome()
        .expect("loop exits only on a terminal session, which always has an outcome"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use myna_core::{AudioFormat, PcmChunk};

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

    #[tokio::test]
    async fn happy_path_full_status_sequence() {
        let backend = FakeBackend::happy_path();
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(&backend, SessionConfig::default(), in_rx, out_tx).await
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
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(&backend, SessionConfig::default(), in_rx, out_tx).await
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
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(&backend, SessionConfig::default(), in_rx, out_tx).await
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
        let (out_tx, out_rx) = mpsc::channel(64);

        let driver = tokio::spawn(async move {
            run_session(&backend, SessionConfig::default(), in_rx, out_tx).await
        });

        in_tx.send(OrchestratorInput::Audio(chunk())).await.unwrap();
        in_tx.send(OrchestratorInput::Abort).await.unwrap();
        drop(in_tx);

        let outcome = driver.await.unwrap().expect("session opened");
        let _ = collect(out_rx).await;
        assert_eq!(outcome, SessionOutcome::Aborted);
    }
}
