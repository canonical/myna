//! WebSocket-over-Unix-domain-socket [`BackendClient`] — speaks the *existing*
//! `myna.core` wire (see `myna/core/transport_ws.py`), so the orchestrator can
//! run against the live Python `myna-server` today. IE115 event names layer on
//! as a second backend later (plan T43); the FSM above this doesn't change.
//!
//! Framing: one WS connection per session; `session.start` up, then PCM as
//! binary frames, `session.finish` at end-of-audio; transcript events come down
//! as JSON text frames, ending in exactly one terminal event.

use futures_util::{SinkExt, StreamExt};
use myna_core::{
    ClientControl, SessionConfig, TranscriptionEvent, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{BackendClient, BackendError, BackendEvents, BackendHandle, BackendSink, Outbound};

/// Bounds the audio backlog held in memory before backpressure kicks in
/// (~1.6 s at 100 ms chunks) — the "bounded buffer" invariant.
const OUTBOUND_CAPACITY: usize = 16;
/// Transcript events are small and infrequent; a little slack avoids stalling
/// the reader behind a slow consumer.
const EVENT_CAPACITY: usize = 64;

/// The connection URL is a formality over a Unix socket (there is no host);
/// the Python server ignores the path and only needs a valid WS upgrade.
const WS_URL: &str = "ws://localhost/";

/// Connects to a `myna-server` (or inference snap) listening on a Unix socket.
pub struct WsUnixBackend {
    socket_path: std::path::PathBuf,
}

impl WsUnixBackend {
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Self {
        Self { socket_path: socket_path.into() }
    }
}

#[async_trait::async_trait]
impl BackendClient for WsUnixBackend {
    async fn open_session(&self, config: SessionConfig) -> Result<BackendHandle, BackendError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| BackendError::Connect(format!("{}: {e}", self.socket_path.display())))?;
        let (ws, _resp) = tokio_tungstenite::client_async(WS_URL, stream)
            .await
            .map_err(|e| BackendError::Handshake(e.to_string()))?;
        let (mut write, mut read) = ws.split();

        // Declare protocol + config.
        let start = ClientControl::SessionStart {
            protocol_version: PROTOCOL_VERSION.to_string(),
            config,
        };
        write
            .send(Message::Text(serde_json::to_string(&start).expect("serializable")))
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        // Await the handshake reply: `session.created` (success) or a terminal
        // `transcription.error` (rejection, e.g. bad protocol version).
        let protocol_version = read_handshake_ack(&mut read).await?;

        let (out_tx, out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let (ev_tx, ev_rx) = mpsc::channel::<Result<TranscriptionEvent, BackendError>>(EVENT_CAPACITY);
        tokio::spawn(pump(write, read, out_rx, ev_tx));

        Ok(BackendHandle {
            sink: BackendSink { tx: out_tx },
            events: BackendEvents { rx: ev_rx },
            protocol_version,
        })
    }
}

type WsRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
>;
type WsWrite = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    Message,
>;

/// Read frames until the `session.created` ack; return the acknowledged
/// protocol version. A `transcription.error` before it is a rejection.
async fn read_handshake_ack(read: &mut WsRead) -> Result<Option<String>, BackendError> {
    loop {
        match read.next().await {
            Some(Ok(Message::Text(text))) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| BackendError::Handshake(format!("bad JSON: {e}")))?;
                if value.get("type").and_then(Value::as_str) == Some("session.created") {
                    return Ok(value
                        .get("protocol_version")
                        .and_then(Value::as_str)
                        .map(String::from));
                }
                if value.get("event").is_some() {
                    // The server can reject the session with a terminal error
                    // *instead* of `session.created` (e.g. bad version).
                    if let Some(TranscriptionEvent::Error(err)) =
                        TranscriptionEvent::from_wire(&value)?
                    {
                        return Err(BackendError::Rejected { code: err.code, message: err.message });
                    }
                    return Err(BackendError::Handshake(
                        "transcript event before session.created".into(),
                    ));
                }
                return Err(BackendError::Handshake(format!("unexpected frame: {text}")));
            }
            Some(Ok(Message::Close(_))) | None => return Err(BackendError::Closed),
            Some(Ok(_)) => continue, // binary/ping/pong before the ack: ignore
            Some(Err(e)) => return Err(BackendError::Transport(e.to_string())),
        }
    }
}

/// Owns the split WS stream for the session's lifetime: pumps outbound audio /
/// control up and transcript events down, until a terminal event, an abort, or
/// the connection closes.
async fn pump(
    mut write: WsWrite,
    mut read: WsRead,
    mut out_rx: mpsc::Receiver<Outbound>,
    ev_tx: mpsc::Sender<Result<TranscriptionEvent, BackendError>>,
) {
    // Once the FSM drops the sink (without an explicit Abort), we stop sending
    // but keep draining events until the backend finishes (commit-drain).
    let mut outbound_open = true;
    loop {
        tokio::select! {
            outbound = out_rx.recv(), if outbound_open => match outbound {
                Some(Outbound::Audio(chunk)) => {
                    if write.send(Message::Binary(chunk.data.to_vec())).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Finish) => {
                    let frame = serde_json::to_string(&ClientControl::SessionFinish)
                        .expect("serializable");
                    if write.send(Message::Text(frame)).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Abort) => {
                    let _ = write.close().await;
                    return; // abort: stop reading events too
                }
                None => outbound_open = false, // sink dropped; keep reading
            },
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match decode_event(&text) {
                        Ok(Some(event)) => {
                            let terminal = event.is_terminal();
                            if ev_tx.send(Ok(event)).await.is_err() {
                                break; // FSM dropped the receiver
                            }
                            if terminal {
                                break;
                            }
                        }
                        Ok(None) => {} // stray control frame: ignore
                        Err(e) => {
                            let _ = ev_tx.send(Err(e)).await;
                            break;
                        }
                    }
                }
                Some(Ok(Message::Binary(_))) => {} // server never sends binary
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong/frame handled by the library
                Some(Err(e)) => {
                    let _ = ev_tx.send(Err(BackendError::Transport(e.to_string()))).await;
                    break;
                }
            },
        }
    }
}

/// Decode a downstream text frame into a transcript event, or `None` if it is a
/// control frame or an unknown (additive) event type we should ignore.
fn decode_event(text: &str) -> Result<Option<TranscriptionEvent>, BackendError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| BackendError::Transport(format!("bad JSON: {e}")))?;
    if value.get("event").is_some() {
        Ok(TranscriptionEvent::from_wire(&value)?)
    } else {
        Ok(None)
    }
}
