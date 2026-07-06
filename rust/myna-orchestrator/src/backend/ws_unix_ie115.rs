//! IE115-dialect [`BackendClient`] — the OpenAI-Realtime-shaped wire (plan T43),
//! the Rust mirror of Python `myna.core.wire_ie115` + `WsUnixIe115Client`.
//!
//! Same transport (WebSocket over a Unix socket), different frame vocabulary.
//! The FSM above this is **unchanged** — this is a second backend behind the
//! same [`BackendClient`] trait, which is the whole point of the wire-agnostic
//! FSM (T40). See `docs/architecture/ie115-wire.md` for the frame contract.
//!
//! Client→server: `session.update` (nested config) up front, then PCM as raw
//! binary frames (default) or base64 `input_audio_buffer.append` (OpenAI-parity,
//! `--base64-audio`), then `input_audio_buffer.commit` at end-of-audio.
//! Server→client: `session.created`/`session.updated` (ignored control frames),
//! additive `STATUS{state}` liveness, committed `…transcription.delta` segments
//! (→ `final`), the utterance's `…transcription.completed` (→ `done`, the
//! terminal), and `error`.
//!
//! The server keeps the connection open across commits (OpenAI multi-commit
//! shape, decided 2026-07-06); this client uses one commit per connection and
//! closes after its `completed` arrives. A close *before* the terminal ends the
//! event stream without one, which the FSM maps to a `connection_closed`
//! failure — never a synthesised `done` (a dead server must not read as a
//! successful, possibly truncated, utterance).

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use myna_core::{
    ErrorData, PcmChunk, Progress, SessionConfig, TranscriptionEvent, TranscriptionFinal,
    PHASE_PREPARING, PHASE_READY, PHASE_TRANSCRIBING,
};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{BackendClient, BackendError, BackendEvents, BackendHandle, BackendSink, Outbound};

const OUTBOUND_CAPACITY: usize = 16;
const EVENT_CAPACITY: usize = 64;
const WS_URL: &str = "ws://localhost/";

// IE115 frame type strings.
const SESSION_UPDATE: &str = "session.update";
const INPUT_AUDIO_APPEND: &str = "input_audio_buffer.append";
const INPUT_AUDIO_COMMIT: &str = "input_audio_buffer.commit";
const STATUS_EVENT: &str = "status";
const TRANSCRIPTION_DELTA: &str = "conversation.item.input_audio_transcription.delta";
const TRANSCRIPTION_COMPLETED: &str = "conversation.item.input_audio_transcription.completed";
const ERROR: &str = "error";

/// Connects to an IE115-speaking server on a Unix socket. `base64_audio` selects
/// the OpenAI-parity append path (base64-in-JSON) over raw binary frames.
pub struct WsUnixIe115Backend {
    socket_path: std::path::PathBuf,
    base64_audio: bool,
}

impl WsUnixIe115Backend {
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Self {
        Self { socket_path: socket_path.into(), base64_audio: false }
    }

    /// Send audio as base64 `input_audio_buffer.append` frames (OpenAI parity)
    /// instead of raw WS binary frames.
    pub fn base64_audio(mut self, yes: bool) -> Self {
        self.base64_audio = yes;
        self
    }
}

/// Build the IE115 `session.update` frame from a flat [`SessionConfig`] (mirror
/// of Python `session_config_to_ie115`).
fn session_update_frame(config: &SessionConfig) -> Value {
    let mut transcription = serde_json::Map::new();
    if let Some(language) = &config.language {
        transcription.insert("language".into(), json!(language));
    }
    if let Some(prompt) = &config.prompt {
        transcription.insert("prompt".into(), json!(prompt));
    }
    json!({
        "type": SESSION_UPDATE,
        "session": {
            "type": "realtime",
            "audio": { "input": {
                "format": { "type": "audio/pcm", "rate": config.audio_format.sample_rate_hz },
                "transcription": Value::Object(transcription),
            }},
        }
    })
}

#[async_trait::async_trait]
impl BackendClient for WsUnixIe115Backend {
    async fn open_session(&self, config: SessionConfig) -> Result<BackendHandle, BackendError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| BackendError::Connect(format!("{}: {e}", self.socket_path.display())))?;
        let (ws, _resp) = tokio_tungstenite::client_async(WS_URL, stream)
            .await
            .map_err(|e| BackendError::Handshake(e.to_string()))?;
        let (mut write, read) = ws.split();

        // Send `session.update` eagerly — the shape-sniff trigger. Like the
        // Python client we do not block on `session.created`; the pump ignores
        // the control frames.
        let frame = session_update_frame(&config);
        write
            .send(Message::Text(frame.to_string()))
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        let (out_tx, out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let (ev_tx, ev_rx) = mpsc::channel::<Result<TranscriptionEvent, BackendError>>(EVENT_CAPACITY);
        tokio::spawn(pump(write, read, out_rx, ev_tx, self.base64_audio));

        Ok(BackendHandle {
            sink: BackendSink { tx: out_tx },
            events: BackendEvents { rx: ev_rx },
            protocol_version: None, // IE115 carries no protocol_version
        })
    }
}

type WsRead =
    futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>>;
type WsWrite = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    Message,
>;

/// Decode one IE115 server frame into zero or more internal events (stateless —
/// the utterance terminal is a real frame, `completed` → `done`). Control frames
/// (`session.created`/`session.updated`) yield nothing. Mirrors the Python
/// `Ie115Decoder`.
fn decode_frame(value: &Value) -> Vec<TranscriptionEvent> {
    match value.get("type").and_then(Value::as_str) {
        Some(STATUS_EVENT) => {
            let phase = match value.get("state").and_then(Value::as_str) {
                Some("loading") => PHASE_PREPARING,
                Some("ready") => PHASE_READY,
                _ => PHASE_TRANSCRIBING,
            };
            let snippet = value.get("snippet").and_then(Value::as_str).map(String::from);
            vec![TranscriptionEvent::Progress(Progress { snippet, phase: phase.to_string() })]
        }
        Some(TRANSCRIPTION_DELTA) => {
            // Committed, append-only segment text — the IE115 face of
            // `transcription.final` (streaming contract, streaming.md §3a).
            let text = value.get("delta").and_then(Value::as_str).unwrap_or("").to_string();
            vec![TranscriptionEvent::Final(TranscriptionFinal { text, segments: vec![] })]
        }
        Some(TRANSCRIPTION_COMPLETED) => {
            // The utterance terminal: full transcript for this commit.
            let text = value.get("transcript").and_then(Value::as_str).unwrap_or("").to_string();
            vec![TranscriptionEvent::Done(TranscriptionFinal { text, segments: vec![] })]
        }
        Some(ERROR) => {
            let err = value.get("error");
            let code = err
                .and_then(|e| e.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("server_error")
                .to_string();
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![TranscriptionEvent::Error(ErrorData { code, message })]
        }
        _ => vec![], // session.created/updated or unknown additive frame
    }
}

/// Encode a PCM chunk as an `input_audio_buffer.append` frame (base64).
fn append_frame(chunk: &PcmChunk) -> String {
    let audio = base64::engine::general_purpose::STANDARD.encode(&chunk.data);
    json!({ "type": INPUT_AUDIO_APPEND, "audio": audio }).to_string()
}

async fn pump(
    mut write: WsWrite,
    mut read: WsRead,
    mut out_rx: mpsc::Receiver<Outbound>,
    ev_tx: mpsc::Sender<Result<TranscriptionEvent, BackendError>>,
    base64_audio: bool,
) {
    let mut outbound_open = true;
    loop {
        tokio::select! {
            outbound = out_rx.recv(), if outbound_open => match outbound {
                Some(Outbound::Audio(chunk)) => {
                    let msg = if base64_audio {
                        Message::Text(append_frame(&chunk))
                    } else {
                        Message::Binary(chunk.data.to_vec())
                    };
                    if write.send(msg).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Finish) => {
                    let frame = json!({ "type": INPUT_AUDIO_COMMIT }).to_string();
                    if write.send(Message::Text(frame)).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Abort) => {
                    let _ = write.close().await;
                    return;
                }
                None => outbound_open = false, // sink dropped; keep reading (commit-drain)
            },
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = ev_tx.send(Err(BackendError::Transport(format!("bad JSON: {e}")))).await;
                            break;
                        }
                    };
                    for event in decode_frame(&value) {
                        let terminal = event.is_terminal();
                        if ev_tx.send(Ok(event)).await.is_err() {
                            return; // FSM dropped the receiver
                        }
                        if terminal {
                            // Our commit is answered; close our side of the
                            // persistent connection (one utterance per connection).
                            return;
                        }
                    }
                }
                Some(Ok(Message::Binary(_))) => {} // server never sends binary
                // Close before the terminal: fall out and drop `ev_tx` — the
                // ended event stream reaches the FSM as BackendClosed (a
                // `connection_closed` failure), never a synthesised `done`.
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong
                Some(Err(e)) => {
                    let _ = ev_tx.send(Err(BackendError::Transport(e.to_string()))).await;
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_update_carries_nested_config() {
        let cfg = SessionConfig {
            language: Some("en".into()),
            prompt: Some("nouns".into()),
            ..Default::default()
        };
        let frame = session_update_frame(&cfg);
        assert_eq!(frame["type"], SESSION_UPDATE);
        let input = &frame["session"]["audio"]["input"];
        assert_eq!(input["format"]["rate"], 16_000);
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["transcription"]["language"], "en");
        assert_eq!(input["transcription"]["prompt"], "nouns");
    }

    #[test]
    fn decoder_maps_status_to_progress_phases() {
        let loading = decode_frame(&json!({"type": "status", "state": "loading"}));
        assert!(matches!(&loading[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_PREPARING));
        let ready = decode_frame(&json!({"type": "status", "state": "ready"}));
        assert!(matches!(&ready[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_READY));
    }

    #[test]
    fn decoder_delta_is_committed_final() {
        let f = decode_frame(&json!({
            "type": TRANSCRIPTION_DELTA, "item_id": "i1", "content_index": 0,
            "delta": "one"
        }));
        assert!(matches!(&f[0], TranscriptionEvent::Final(t) if t.text == "one"));
    }

    #[test]
    fn decoder_completed_is_the_terminal_done() {
        let done = decode_frame(&json!({
            "type": TRANSCRIPTION_COMPLETED, "item_id": "i1", "content_index": 0,
            "transcript": "one two"
        }));
        assert!(matches!(&done[0], TranscriptionEvent::Done(t) if t.text == "one two"));
        assert!(done[0].is_terminal());
    }

    #[test]
    fn decoder_error_is_terminal() {
        let e = decode_frame(&json!({
            "type": ERROR, "error": {"type": "server_error", "code": "server_error", "message": "boom"}
        }));
        assert!(matches!(&e[0], TranscriptionEvent::Error(err) if err.code == "server_error"));
        assert!(e[0].is_terminal());
    }

    #[test]
    fn decoder_ignores_control_frames() {
        assert!(decode_frame(&json!({"type": "session.created", "session": {}})).is_empty());
        assert!(decode_frame(&json!({"type": "session.updated", "session": {}})).is_empty());
    }
}
