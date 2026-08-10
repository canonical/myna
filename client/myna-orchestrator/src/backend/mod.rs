//! The inference-backend boundary — the seam between the orchestrator FSM and
//! the STT service (the inference snap, or the Python `myna-server` standing in
//! for it during development).
//!
//! The FSM never touches a socket: it drives a [`BackendClient`], which yields a
//! [`BackendHandle`] split into a cheap-clone [`BackendSink`] (audio + control
//! *up*) and a [`BackendEvents`] receiver (transcript events *down*). That split
//! is what lets the FSM push audio and consume events **concurrently** over one
//! session, and it decouples the FSM from the wire entirely: the WS-over-UDS
//! client ([`ws_unix`]) and the T40 fake backend implement the same trait over
//! the same channels.

pub mod fake;
pub mod ws_unix;
pub mod ws_unix_ie115;

use async_trait::async_trait;
use myna_core::{PcmChunk, SessionConfig, TranscriptionEvent, WireError};
use thiserror::Error;
use tokio::sync::mpsc;

/// A client that opens transcription sessions against an STT backend.
#[async_trait]
pub trait BackendClient: Send + Sync {
    /// Open a session: perform the handshake (declare the protocol version,
    /// send the config, await the `session.created` ack) and return a handle
    /// ready to stream audio and receive events. Fails if the backend rejects
    /// the session (e.g. unsupported protocol version) or can't be reached.
    async fn open_session(&self, config: SessionConfig) -> Result<BackendHandle, BackendError>;
}

/// What the FSM sends *up* to the backend over a session. Mirrors the client
/// side of the ws+unix wire: PCM binary frames, then a `session.finish` control
/// frame at end-of-audio — or an abort (close without finish).
#[derive(Debug)]
pub enum Outbound {
    /// A chunk of PCM to transcribe (goes out as a binary frame).
    Audio(PcmChunk),
    /// End of audio, hotkey released (`session.finish`). The backend keeps
    /// decoding the tail and finishes with a terminal event — see the
    /// commit-drain edge case in `docs/architecture/ie115-lifecycle.md` §3C.
    Finish,
    /// Abandon the session: close the connection without finishing. The backend
    /// commits nothing.
    Abort,
}

/// Failure interacting with the backend.
#[derive(Debug, Error)]
pub enum BackendError {
    #[error("cannot reach backend: {0}")]
    Connect(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
    /// The backend refused the session with a terminal error during the
    /// handshake (e.g. `unsupported_protocol_version`).
    #[error("session rejected: {code}: {message}")]
    Rejected { code: String, message: String },
    #[error("malformed event from backend: {0}")]
    Wire(#[from] WireError),
    #[error("backend connection closed unexpectedly")]
    Closed,
    #[error("transport error: {0}")]
    Transport(String),
}

/// The audio/control side of an open session. Cheap to clone (it is a channel
/// sender), so the FSM can hand a clone to an audio-pump task while it consumes
/// events elsewhere.
#[derive(Clone)]
pub struct BackendSink {
    tx: mpsc::Sender<Outbound>,
}

impl BackendSink {
    /// Push a PCM chunk to the backend. Applies backpressure (the channel is
    /// bounded — the "bounded in-memory buffer" invariant); errors only if the
    /// session's transport task has gone away.
    pub async fn send_audio(&self, chunk: PcmChunk) -> Result<(), BackendError> {
        self.tx
            .send(Outbound::Audio(chunk))
            .await
            .map_err(|_| BackendError::Closed)
    }

    /// Signal end-of-audio (`session.finish`). The session is *not* over — the
    /// FSM must keep consuming events until a terminal one arrives.
    pub async fn finish(&self) -> Result<(), BackendError> {
        self.tx
            .send(Outbound::Finish)
            .await
            .map_err(|_| BackendError::Closed)
    }

    /// Abort the session (close without finishing); nothing is committed.
    pub async fn abort(&self) -> Result<(), BackendError> {
        self.tx
            .send(Outbound::Abort)
            .await
            .map_err(|_| BackendError::Closed)
    }
}

/// The event side of an open session: transcript events flow down until a
/// terminal event ([`TranscriptionEvent::is_terminal`]) or the connection
/// closes (then `None`).
pub struct BackendEvents {
    rx: mpsc::Receiver<Result<TranscriptionEvent, BackendError>>,
}

impl BackendEvents {
    /// Await the next event. `None` means the stream ended (terminal event
    /// already delivered, or the connection closed).
    pub async fn next(&mut self) -> Option<Result<TranscriptionEvent, BackendError>> {
        self.rx.recv().await
    }
}

/// A live session: the two halves plus the protocol version the backend
/// acknowledged in `session.created` (`None` from a pre-versioning peer).
pub struct BackendHandle {
    pub sink: BackendSink,
    pub events: BackendEvents,
    protocol_version: Option<String>,
}

impl BackendHandle {
    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    /// Take the two halves apart for independent, concurrent use.
    pub fn split(self) -> (BackendSink, BackendEvents, Option<String>) {
        (self.sink, self.events, self.protocol_version)
    }
}
