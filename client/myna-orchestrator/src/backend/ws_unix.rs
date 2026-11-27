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
    Capabilities, ClientControl, ServerControl, SessionConfig, TranscriptionEvent, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;

use super::transport::{self, Dialect, Ws};
use super::{BackendClient, BackendError, BackendHandle, Outbound};

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
        Self {
            socket_path: socket_path.into(),
        }
    }
}

#[async_trait::async_trait]
impl BackendClient for WsUnixBackend {
    async fn open_session(&self, config: SessionConfig) -> Result<BackendHandle, BackendError> {
        let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            myna_core::info_log!(
                "backend",
                "connect FAILED: {}: {e}",
                self.socket_path.display()
            );
            BackendError::Connect(format!("{}: {e}", self.socket_path.display()))
        })?;
        myna_core::info_log!("backend", "connected to {}", self.socket_path.display());
        let (mut ws, _resp) = tokio_tungstenite::client_async(WS_URL, stream)
            .await
            .map_err(|e| BackendError::Handshake(e.to_string()))?;

        // Declare protocol + config.
        let start = ClientControl::SessionStart {
            protocol_version: PROTOCOL_VERSION.to_string(),
            config,
        };
        ws.send(Message::text(
            serde_json::to_string(&start).expect("serializable"),
        ))
        .await
        .map_err(|e| BackendError::Transport(e.to_string()))?;

        // Await the handshake reply: `session.created` (success) or a terminal
        // `transcription.error` (rejection, e.g. bad protocol version).
        let protocol_version = read_handshake_ack(&mut ws).await?;

        let (sink, events) = transport::spawn(ws, Internal, OUTBOUND_CAPACITY, EVENT_CAPACITY);
        Ok(BackendHandle::new(sink, events, protocol_version))
    }
}

/// Ask a backend what it can do, outside of any session (T24 discovery) — what
/// `myna-desktop --status` uses to name the model actually being served,
/// since the resolved socket path itself carries no identity (the content
/// share's mount-point basename is snapd's, not the backend's).
///
/// One connection, used once and discarded: skip past the eager
/// `session.created` greeting, send `capabilities.query` as the shape-sniffed
/// first frame (`transport_ws.py`'s `_SessionHandler.handle`), and return what
/// comes back. Doesn't open a session, so it can't wake a lazily-loaded model.
///
/// Bounded by [`CAPABILITIES_TIMEOUT`]: a wedged backend is an error, not a
/// hang.
pub async fn query_capabilities(
    socket_path: &std::path::Path,
) -> Result<Capabilities, BackendError> {
    query_capabilities_within(socket_path, CAPABILITIES_TIMEOUT).await
}

/// How long [`query_capabilities`] waits. Local socket, answered from the
/// greeting's loop, so anything near this is a wedged server.
pub const CAPABILITIES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn query_capabilities_within(
    socket_path: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<Capabilities, BackendError> {
    tokio::time::timeout(timeout, query_capabilities_unbounded(socket_path))
        .await
        .unwrap_or_else(|_| {
            Err(BackendError::Transport(format!(
                "no capabilities reply within {timeout:?}"
            )))
        })
}

async fn query_capabilities_unbounded(
    socket_path: &std::path::Path,
) -> Result<Capabilities, BackendError> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| BackendError::Connect(format!("{}: {e}", socket_path.display())))?;
    let (ws, _resp) = tokio_tungstenite::client_async(WS_URL, stream)
        .await
        .map_err(|e| BackendError::Handshake(e.to_string()))?;
    let (mut write, mut read) = ws.split();

    // The greeting is always the first frame; skip it (and any stray
    // ping/pong ahead of it) rather than parse it, same as `read_handshake_ack`.
    loop {
        match read.next().await {
            Some(Ok(Message::Text(_))) => break,
            Some(Ok(Message::Close(_))) | None => return Err(BackendError::Closed),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(BackendError::Transport(e.to_string())),
        }
    }

    write
        .send(Message::text(
            serde_json::to_string(&ClientControl::CapabilitiesQuery).expect("serializable"),
        ))
        .await
        .map_err(|e| BackendError::Transport(e.to_string()))?;

    loop {
        match read.next().await {
            Some(Ok(Message::Text(text))) => {
                return match serde_json::from_str(&text) {
                    Ok(ServerControl::Capabilities { data }) => Ok(data),
                    Ok(other) => Err(BackendError::Handshake(format!(
                        "expected capabilities, got {other:?}"
                    ))),
                    Err(e) => Err(BackendError::Handshake(format!("bad JSON: {e}"))),
                };
            }
            Some(Ok(Message::Close(_))) | None => return Err(BackendError::Closed),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(BackendError::Transport(e.to_string())),
        }
    }
}

/// Read frames until the `session.created` ack; return the acknowledged
/// protocol version. A `transcription.error` before it is a rejection.
async fn read_handshake_ack(ws: &mut Ws) -> Result<Option<String>, BackendError> {
    loop {
        match ws.next().await {
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
                        return Err(BackendError::Rejected {
                            code: err.code,
                            message: err.message,
                        });
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

/// The internal `myna.core` framing: PCM as binary frames, `session.finish`,
/// and `{"event": …}` transcript frames down.
struct Internal;

impl Dialect for Internal {
    fn encode(&mut self, item: Outbound) -> Message {
        match item {
            Outbound::Audio(chunk) => Message::binary(chunk.data),
            Outbound::Finish => {
                myna_core::dbg_log!("ws", "-> session.finish");
                Message::text(
                    serde_json::to_string(&ClientControl::SessionFinish).expect("serializable"),
                )
            }
        }
    }

    fn decode(&mut self, text: &str) -> Result<Vec<TranscriptionEvent>, BackendError> {
        let event = decode_event(text)?;
        if let Some(event) = &event {
            myna_core::dbg_log!("ws", "<- {}", event_summary(event));
        }
        Ok(event.into_iter().collect())
    }
}

/// Decode a downstream text frame into a transcript event, or `None` if it is a
/// control frame or an unknown (additive) event type we should ignore.
fn decode_event(text: &str) -> Result<Option<TranscriptionEvent>, BackendError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| BackendError::Transport(format!("bad JSON: {e}")))?;
    if value.get("event").is_some() {
        Ok(TranscriptionEvent::from_wire(&value)?)
    } else {
        Ok(None)
    }
}

/// One-line summary of a wire event for `MYNA_DEBUG`: its type and the
/// length of its text, never the text, which is the user's.
pub(crate) fn event_summary(event: &TranscriptionEvent) -> String {
    match event {
        TranscriptionEvent::Progress(p) => format!("progress(phase={})", p.phase),
        TranscriptionEvent::Final(f) => format!("final(len={})", f.text.len()),
        TranscriptionEvent::Done(d) => format!("done(len={})", d.text.len()),
        TranscriptionEvent::Error(e) => format!("error(code={}, {})", e.code, e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_summary_never_carries_the_dictated_text() {
        let said = myna_core::events::TranscriptionFinal {
            text: "my secret sentence".into(),
            ..Default::default()
        };
        for event in [
            TranscriptionEvent::Final(said.clone()),
            TranscriptionEvent::Done(said),
        ] {
            let summary = event_summary(&event);
            assert!(summary.contains("len=18"), "{summary}");
            assert!(!summary.contains("secret"), "{summary}");
        }
    }

    /// Accepts and upgrades, then never sends the greeting.
    async fn wedged_backend(tag: &str) -> (std::path::PathBuf, tokio::task::JoinHandle<()>) {
        let path =
            std::env::temp_dir().join(format!("myna-orch-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(tokio_tungstenite::accept_async(stream).await);
            }
        });
        (path, task)
    }

    #[tokio::test]
    async fn a_wedged_backend_is_an_error_not_a_hang() {
        let (path, task) = wedged_backend("wedged").await;
        let started = std::time::Instant::now();
        let got = query_capabilities_within(&path, std::time::Duration::from_millis(200)).await;
        task.abort();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(got, Err(BackendError::Transport(_))), "{got:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }
}
