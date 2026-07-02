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
//! additive `STATUS{state}` liveness, `conversation.item.input_audio_transcription.completed`,
//! and `error`. IE115 has no session-level terminal, so a `done` is synthesised
//! from the accumulated completes when the connection closes (note §7.4).

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

/// Accumulates `completed` transcripts so a terminal `done` can be synthesised
/// on close (IE115 has no session terminal — note §7.4). Mirrors the Python
/// `Ie115Decoder`.
struct Decoder {
    transcripts: Vec<String>,
    terminated: bool,
}

impl Decoder {
    fn new() -> Self {
        Self { transcripts: Vec::new(), terminated: false }
    }

    /// Decode one IE115 server frame into zero or more internal events. Control
    /// frames (`session.created`/`session.updated`) yield nothing.
    fn decode(&mut self, value: &Value) -> Vec<TranscriptionEvent> {
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
                // IE115 delta is committed-incremental; we have no committed-delta
                // semantics yet (T08), so surface it as an unstable snippet.
                let snippet = value.get("delta").and_then(Value::as_str).map(String::from);
                vec![TranscriptionEvent::Progress(Progress {
                    snippet,
                    phase: PHASE_TRANSCRIBING.to_string(),
                })]
            }
            Some(TRANSCRIPTION_COMPLETED) => {
                let text =
                    value.get("transcript").and_then(Value::as_str).unwrap_or("").to_string();
                self.transcripts.push(text.clone());
                vec![TranscriptionEvent::Final(TranscriptionFinal { text, segments: vec![] })]
            }
            Some(ERROR) => {
                self.terminated = true;
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

    /// Synthesise the terminal `done` from accumulated completes, unless a
    /// terminal `error` already went out.
    fn on_close(&mut self) -> Vec<TranscriptionEvent> {
        if self.terminated {
            return vec![];
        }
        self.terminated = true;
        let text = self
            .transcripts
            .iter()
            .filter(|t| !t.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        vec![TranscriptionEvent::Done(TranscriptionFinal { text, segments: vec![] })]
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
    let mut decoder = Decoder::new();
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
                    let mut terminal = false;
                    for event in decoder.decode(&value) {
                        terminal = event.is_terminal();
                        if ev_tx.send(Ok(event)).await.is_err() {
                            return; // FSM dropped the receiver
                        }
                        if terminal {
                            break;
                        }
                    }
                    if terminal {
                        return; // terminal already delivered; no synthesised done
                    }
                }
                Some(Ok(Message::Binary(_))) => {} // server never sends binary
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong
                Some(Err(e)) => {
                    let _ = ev_tx.send(Err(BackendError::Transport(e.to_string()))).await;
                    return;
                }
            },
        }
    }
    // Connection closed without an IE115 terminal frame → synthesise `done`.
    for event in decoder.on_close() {
        let _ = ev_tx.send(Ok(event)).await;
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
        let mut d = Decoder::new();
        let loading = d.decode(&json!({"type": "status", "state": "loading"}));
        assert!(matches!(&loading[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_PREPARING));
        let ready = d.decode(&json!({"type": "status", "state": "ready"}));
        assert!(matches!(&ready[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_READY));
    }

    #[test]
    fn decoder_completed_to_final_then_synthesises_done() {
        let mut d = Decoder::new();
        let f = d.decode(&json!({
            "type": TRANSCRIPTION_COMPLETED, "item_id": "i1", "content_index": 0,
            "transcript": "one"
        }));
        assert!(matches!(&f[0], TranscriptionEvent::Final(t) if t.text == "one"));
        d.decode(&json!({
            "type": TRANSCRIPTION_COMPLETED, "item_id": "i2", "content_index": 0,
            "transcript": "two"
        }));
        let done = d.on_close();
        assert!(matches!(&done[0], TranscriptionEvent::Done(t) if t.text == "one two"));
    }

    #[test]
    fn decoder_error_is_terminal_and_suppresses_synthesised_done() {
        let mut d = Decoder::new();
        let e = d.decode(&json!({
            "type": ERROR, "error": {"type": "server_error", "code": "server_error", "message": "boom"}
        }));
        assert!(matches!(&e[0], TranscriptionEvent::Error(err) if err.code == "server_error"));
        assert!(d.on_close().is_empty());
    }

    #[test]
    fn decoder_ignores_control_frames() {
        let mut d = Decoder::new();
        assert!(d.decode(&json!({"type": "session.created", "session": {}})).is_empty());
        assert!(d.decode(&json!({"type": "session.updated", "session": {}})).is_empty());
    }
}
