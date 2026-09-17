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
pub mod share;
mod transport;
pub mod ws_unix;
pub mod ws_unix_ie115;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use myna_core::{PcmChunk, SessionConfig, TranscriptionEvent, WireError};
use tokio::sync::{mpsc, watch};

use crate::i18n::tr;
use crate::task::TaskGuard;

/// A client that opens transcription sessions against an STT backend.
#[async_trait]
pub trait BackendClient: Send + Sync {
    /// Open a session: perform the handshake (declare the protocol version,
    /// send the config, await the `session.created` ack) and return a handle
    /// ready to stream audio and receive events. Fails if the backend rejects
    /// the session (e.g. unsupported protocol version) or can't be reached.
    async fn open_session(&self, config: SessionConfig) -> Result<BackendHandle, BackendError>;
}

/// What the FSM sends *up* to the backend over a session, in order. Mirrors
/// the client side of the ws+unix wire: PCM binary frames, then a
/// `session.finish` control frame at end-of-audio. Abort is not queued here:
/// it is out of band ([`BackendSink::abort`]).
#[derive(Debug)]
pub enum Outbound {
    /// A chunk of PCM to transcribe (goes out as a binary frame).
    Audio(PcmChunk),
    /// End of audio, hotkey released (`session.finish`). The backend keeps
    /// decoding the tail and finishes with a terminal event.
    Finish,
}

/// Failure interacting with the backend.
///
/// `Display` renders user-visible messages through this crate's gettext domain
/// ([`crate::i18n`]); with no .mo installed it is the identity, so the English
/// strings below double as the source templates for translation.
#[derive(Debug)]
pub enum BackendError {
    Connect(String),
    Handshake(String),
    /// The backend refused the session with a terminal error during the
    /// handshake (e.g. `unsupported_protocol_version`).
    Rejected {
        code: String,
        message: String,
    },
    Wire(WireError),
    Closed,
    Transport(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Connect(inner) => {
                write!(f, "{}", tr("cannot reach backend: %s").replace("%s", inner))
            }
            BackendError::Handshake(inner) => {
                write!(f, "{}", tr("handshake failed: %s").replace("%s", inner))
            }
            BackendError::Rejected { code, message } => write!(
                f,
                "{}",
                tr("session rejected: %1$s: %2$s")
                    .replace("%1$s", code)
                    .replace("%2$s", message)
            ),
            BackendError::Wire(e) => write!(
                f,
                "{}",
                tr("malformed event from backend: %s").replace("%s", &e.to_string())
            ),
            BackendError::Closed => write!(f, "{}", tr("backend connection closed unexpectedly")),
            BackendError::Transport(inner) => {
                write!(f, "{}", tr("transport error: %s").replace("%s", inner))
            }
        }
    }
}

impl std::error::Error for BackendError {}

impl From<WireError> for BackendError {
    fn from(e: WireError) -> Self {
        BackendError::Wire(e)
    }
}

/// The audio/control side of an open session. Cheap to clone (it is a channel
/// sender), so the FSM can hand a clone to an audio-pump task while it consumes
/// events elsewhere.
#[derive(Clone)]
pub struct BackendSink {
    tx: mpsc::Sender<Outbound>,
    abort: Arc<watch::Sender<bool>>,
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

    /// Wait for room in the outbound queue without committing an item, so a
    /// caller can keep serving other work while the transport is congested.
    pub(crate) async fn reserve(&self) -> Result<mpsc::Permit<'_, Outbound>, BackendError> {
        self.tx.reserve().await.map_err(|_| BackendError::Closed)
    }

    /// Abort the session (close without finishing); nothing is committed.
    /// Never waits: it bypasses queued audio, so it works while the transport
    /// is congested.
    pub fn abort(&self) {
        self.abort.send_replace(true);
    }
}

/// The event side of an open session: transcript events flow down until a
/// terminal event ([`TranscriptionEvent::is_terminal`]) or the connection
/// closes (then `None`). Dropping it cancels the transport task feeding it.
pub struct BackendEvents {
    rx: mpsc::Receiver<Result<TranscriptionEvent, BackendError>>,
    _transport: Option<TaskGuard>,
}

impl BackendEvents {
    /// Await the next event. `None` means the stream ended (terminal event
    /// already delivered, or the connection closed).
    pub async fn next(&mut self) -> Option<Result<TranscriptionEvent, BackendError>> {
        self.rx.recv().await
    }

    /// Tie `transport`'s lifetime to these events.
    pub(crate) fn owning(mut self, transport: TaskGuard) -> Self {
        self._transport = Some(transport);
        self
    }
}

/// Where a transport sends transcript events down to the FSM.
pub(crate) type EventSender = mpsc::Sender<Result<TranscriptionEvent, BackendError>>;

/// The transport's end of a session's outbound queue. The fields are apart so
/// a pump can wait on both at once.
pub(crate) struct Outbox {
    pub(crate) queue: mpsc::Receiver<Outbound>,
    pub(crate) abort: AbortSignal,
}

/// The receiving end of [`BackendSink::abort`].
pub(crate) struct AbortSignal(watch::Receiver<bool>);

impl AbortSignal {
    /// Resolves once the client aborts; never, if every sink goes away
    /// without aborting.
    pub(crate) async fn aborted(&mut self) {
        if self.0.wait_for(|aborted| *aborted).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// The channels of one session: the client halves and the transport's ends.
pub(crate) fn channels(
    outbound_capacity: usize,
    event_capacity: usize,
) -> (BackendSink, Outbox, BackendEvents, EventSender) {
    let (out_tx, out_rx) = mpsc::channel(outbound_capacity);
    let (abort_tx, abort_rx) = watch::channel(false);
    let (ev_tx, ev_rx) = mpsc::channel(event_capacity);
    (
        BackendSink {
            tx: out_tx,
            abort: Arc::new(abort_tx),
        },
        Outbox {
            queue: out_rx,
            abort: AbortSignal(abort_rx),
        },
        BackendEvents {
            rx: ev_rx,
            _transport: None,
        },
        ev_tx,
    )
}

/// A live session: the two halves plus the protocol version the backend
/// acknowledged in `session.created` (`None` from a pre-versioning peer).
pub struct BackendHandle {
    pub sink: BackendSink,
    pub events: BackendEvents,
    protocol_version: Option<String>,
}

impl BackendHandle {
    pub(crate) fn new(
        sink: BackendSink,
        events: BackendEvents,
        protocol_version: Option<String>,
    ) -> Self {
        Self {
            sink,
            events,
            protocol_version,
        }
    }

    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    /// Take the two halves apart for independent, concurrent use.
    pub fn split(self) -> (BackendSink, BackendEvents, Option<String>) {
        (self.sink, self.events, self.protocol_version)
    }
}

#[cfg(test)]
mod tests {
    use super::BackendError;

    // With no catalog installed the domain is the identity, so the rendered
    // message is the English template with its placeholders filled.
    #[test]
    fn errors_render_their_messages_with_placeholders_filled() {
        assert_eq!(
            BackendError::Connect("no socket".into()).to_string(),
            "cannot reach backend: no socket"
        );
        assert_eq!(
            BackendError::Handshake("timeout".into()).to_string(),
            "handshake failed: timeout"
        );
        assert_eq!(
            BackendError::Rejected {
                code: "unsupported_protocol_version".into(),
                message: "want 2".into(),
            }
            .to_string(),
            "session rejected: unsupported_protocol_version: want 2"
        );
        assert_eq!(
            BackendError::Closed.to_string(),
            "backend connection closed unexpectedly"
        );
        assert_eq!(
            BackendError::Transport("reset".into()).to_string(),
            "transport error: reset"
        );
    }
}
