//! IE115-dialect [`BackendClient`] — the OpenAI-Realtime-shaped wire (plan T43),
//! the Rust mirror of Python `myna.core.wire_ie115` + `WsUnixIe115Client`.
//!
//! Same transport (WebSocket over a Unix socket), different frame vocabulary.
//! The FSM above this is **unchanged** — this is a second backend behind the
//! same [`BackendClient`] trait, which is the whole point of the wire-agnostic
//! FSM (T40). This module and its parity tests define the frame contract.
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
use futures_util::SinkExt;
use myna_core::{
    ErrorData, PcmChunk, Progress, SessionConfig, TranscriptionEvent, TranscriptionFinal,
    PHASE_PREPARING, PHASE_READY, PHASE_TRANSCRIBING,
};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;

use super::transport::{self, Dialect};
use super::{BackendClient, BackendError, BackendHandle, Outbound};

const OUTBOUND_CAPACITY: usize = 16;
const EVENT_CAPACITY: usize = 64;
const DEFAULT_WS_PATH: &str = "/";

// IE115 frame type strings.
const SESSION_UPDATE: &str = "session.update";
const INPUT_AUDIO_APPEND: &str = "input_audio_buffer.append";
const INPUT_AUDIO_COMMIT: &str = "input_audio_buffer.commit";
const STATUS_EVENT: &str = "status";
const MODEL_LOADED: &str = "model.loaded";
const MODEL_UNLOADED: &str = "model.unloaded";
const TRANSCRIPTION_DELTA: &str = "conversation.item.input_audio_transcription.delta";
const TRANSCRIPTION_COMPLETED: &str = "conversation.item.input_audio_transcription.completed";
const ERROR: &str = "error";

/// Connects to an IE115-speaking server on a Unix socket. `base64_audio` selects
/// the OpenAI-parity append path (base64-in-JSON) over raw binary frames.
pub struct WsUnixIe115Backend {
    socket_path: std::path::PathBuf,
    base64_audio: bool,
    ws_path: String,
}

impl WsUnixIe115Backend {
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            base64_audio: false,
            ws_path: DEFAULT_WS_PATH.into(),
        }
    }

    /// Send audio as base64 `input_audio_buffer.append` frames (OpenAI parity)
    /// instead of raw WS binary frames.
    pub fn base64_audio(mut self, yes: bool) -> Self {
        self.base64_audio = yes;
        self
    }

    /// Override the WebSocket endpoint path (default `/`). The colleagues'
    /// canonical/whisper-snap adapter serves at `/v1/realtime`.
    pub fn ws_path(mut self, path: impl Into<String>) -> Self {
        self.ws_path = path.into();
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

    let mut input = serde_json::Map::new();
    input.insert(
        "format".into(),
        json!({ "type": "audio/pcm", "rate": config.audio_format.sample_rate_hz }),
    );
    // Only include transcription if non-empty — the canonical/whisper-snap
    // adapter rejects an empty transcription object.
    if !transcription.is_empty() {
        input.insert("transcription".into(), Value::Object(transcription));
    }

    json!({
        "type": SESSION_UPDATE,
        "session": {
            "type": "transcription",
            "audio": { "input": Value::Object(input) },
        }
    })
}

#[async_trait::async_trait]
impl BackendClient for WsUnixIe115Backend {
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
        let ws_url = format!("ws://localhost{}", self.ws_path);
        let (mut ws, _resp) = tokio_tungstenite::client_async(&ws_url, stream)
            .await
            .map_err(|e| BackendError::Handshake(e.to_string()))?;

        // Send `session.update` if we have meaningful config to communicate.
        // For external servers (ws_path != "/") that don't need the shape-sniff
        // trigger, skip it when the update would be empty — their adapters may
        // unconditionally reload the backend on any session.update (observed in
        // canonical/whisper-snap, which kills the connection and loses audio).
        let has_config = config.language.is_some() || config.prompt.is_some();
        let needs_shape_sniff = self.ws_path == DEFAULT_WS_PATH;
        if has_config || needs_shape_sniff {
            let frame = session_update_frame(&config);
            ws.send(Message::text(frame.to_string()))
                .await
                .map_err(|e| BackendError::Transport(e.to_string()))?;
        }

        let dialect = Ie115 {
            base64_audio: self.base64_audio,
            after_commit: false,
        };
        let (sink, events) = transport::spawn(ws, dialect, OUTBOUND_CAPACITY, EVENT_CAPACITY);
        // IE115 carries no protocol_version.
        Ok(BackendHandle::new(sink, events, None))
    }
}

/// Decode one IE115 server frame into zero or more internal events (the
/// utterance terminal is a real frame, `completed` → `done`). Control frames
/// (`session.created`/`session.updated`) yield nothing. Mirrors the Python
/// `Ie115Decoder`. `after_commit` says whether our `input_audio_buffer.commit`
/// has gone out, which is what tells an answered commit from the canonical
/// adapter's mid-stream resets.
fn decode_frame(value: &Value, after_commit: bool) -> Vec<TranscriptionEvent> {
    match value.get("type").and_then(Value::as_str) {
        Some(STATUS_EVENT) => {
            let phase = match value.get("state").and_then(Value::as_str) {
                Some("loading") => PHASE_PREPARING,
                Some("ready") => PHASE_READY,
                _ => PHASE_TRANSCRIBING,
            };
            let snippet = value
                .get("snippet")
                .and_then(Value::as_str)
                .map(String::from);
            vec![TranscriptionEvent::Progress(Progress {
                snippet,
                phase: phase.to_string(),
            })]
        }
        // canonical/whisper-snap adapter: model.loaded ≈ our STATUS{ready}
        Some(MODEL_LOADED) => {
            vec![TranscriptionEvent::Progress(Progress {
                snippet: None,
                phase: PHASE_READY.to_string(),
            })]
        }
        // canonical/whisper-snap adapter: model.unloaded ≈ our STATUS{loading}
        // (their SetConfig unconditionally reloads the backend — closes the gate
        // until the new connection's model.loaded arrives)
        Some(MODEL_UNLOADED) => {
            vec![TranscriptionEvent::Progress(Progress {
                snippet: None,
                phase: PHASE_PREPARING.to_string(),
            })]
        }
        Some(TRANSCRIPTION_DELTA) => {
            // Committed, append-only segment text — the IE115 face of
            // `transcription.final`.
            // Parse disposition field (T12, feature 007); default to committed for backward-compat
            use myna_core::Disposition;
            let text = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let disposition_str = value
                .get("disposition")
                .and_then(Value::as_str)
                .unwrap_or("committed");
            let disposition = if disposition_str == "unstable" {
                Disposition::Unstable
            } else {
                Disposition::Committed
            };
            let segment_index = value
                .get("segment_index")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
            vec![TranscriptionEvent::Final(TranscriptionFinal {
                text,
                segments: vec![],
                disposition,
                segment_index,
            })]
        }
        Some(TRANSCRIPTION_COMPLETED) => {
            // The utterance terminal: full transcript for this commit. Before
            // our commit, though, an empty transcript is the canonical
            // whisper-snap adapter's revision-reset signal (clear the partial,
            // re-send from scratch), not the end of anything. After the
            // commit it is the terminal for an utterance nobody spoke into.
            let text = value
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if text.is_empty() && !after_commit {
                vec![]
            } else {
                vec![TranscriptionEvent::Done(TranscriptionFinal {
                    text,
                    segments: vec![],
                    ..Default::default()
                })]
            }
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
        _ => {
            // session.created/updated or unknown additive frame
            // T015 (feature 007): session.created may carry a "streaming" field
            // indicating whether the server will emit progressive committed segments.
            // TODO: Parse and expose session.streaming when implementing US1.
            vec![]
        }
    }
}

/// Encode a PCM chunk as an `input_audio_buffer.append` frame (base64).
fn append_frame(chunk: &PcmChunk) -> String {
    let audio = base64::engine::general_purpose::STANDARD.encode(&chunk.data);
    json!({ "type": INPUT_AUDIO_APPEND, "audio": audio }).to_string()
}

/// The IE115 framing. `after_commit` flips once our commit has been written,
/// which is what tells an answered commit from a mid-stream reset.
struct Ie115 {
    base64_audio: bool,
    after_commit: bool,
}

impl Dialect for Ie115 {
    fn encode(&mut self, item: Outbound) -> Message {
        match item {
            Outbound::Audio(chunk) if self.base64_audio => Message::text(append_frame(&chunk)),
            Outbound::Audio(chunk) => Message::binary(chunk.data),
            Outbound::Finish => Message::text(json!({ "type": INPUT_AUDIO_COMMIT }).to_string()),
        }
    }

    fn written(&mut self, finish: bool) {
        self.after_commit |= finish;
    }

    fn decode(&mut self, text: &str) -> Result<Vec<TranscriptionEvent>, BackendError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| BackendError::Transport(format!("bad JSON: {e}")))?;
        Ok(decode_frame(&value, self.after_commit))
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
        // OpenAI's transcription session, not its speech-to-speech one.
        assert_eq!(frame["session"]["type"], "transcription");
        let input = &frame["session"]["audio"]["input"];
        assert_eq!(input["format"]["rate"], 16_000);
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["transcription"]["language"], "en");
        assert_eq!(input["transcription"]["prompt"], "nouns");
    }

    #[test]
    fn decoder_maps_status_to_progress_phases() {
        let loading = decode_frame(&json!({"type": "status", "state": "loading"}), false);
        assert!(
            matches!(&loading[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_PREPARING)
        );
        let ready = decode_frame(&json!({"type": "status", "state": "ready"}), false);
        assert!(matches!(&ready[0], TranscriptionEvent::Progress(p) if p.phase == PHASE_READY));
    }

    #[test]
    fn decoder_delta_is_committed_final() {
        let f = decode_frame(
            &json!({
                "type": TRANSCRIPTION_DELTA, "item_id": "i1", "content_index": 0,
                "delta": "one"
            }),
            false,
        );
        assert!(matches!(&f[0], TranscriptionEvent::Final(t) if t.text == "one"));
    }

    #[test]
    fn decoder_completed_is_the_terminal_done() {
        let done = decode_frame(
            &json!({
                "type": TRANSCRIPTION_COMPLETED, "item_id": "i1", "content_index": 0,
                "transcript": "one two"
            }),
            false,
        );
        assert!(matches!(&done[0], TranscriptionEvent::Done(t) if t.text == "one two"));
        assert!(done[0].is_terminal());
    }

    #[test]
    fn empty_completed_is_a_reset_before_our_commit_and_the_terminal_after_it() {
        // The canonical adapter emits empty completeds mid-stream (interop
        // report gap 3); our own server emits one when the user committed
        // without saying anything, and a session that ignored it would hang.
        let frame = json!({
            "type": TRANSCRIPTION_COMPLETED, "item_id": "i1", "content_index": 0,
            "transcript": ""
        });
        assert!(decode_frame(&frame, false).is_empty());
        let done = decode_frame(&frame, true);
        assert!(matches!(&done[0], TranscriptionEvent::Done(t) if t.text.is_empty()));
        assert!(done[0].is_terminal());
    }

    #[test]
    fn decoder_error_is_terminal() {
        let e = decode_frame(
            &json!({
                "type": ERROR, "error": {"type": "server_error", "code": "server_error", "message": "boom"}
            }),
            false,
        );
        assert!(matches!(&e[0], TranscriptionEvent::Error(err) if err.code == "server_error"));
        assert!(e[0].is_terminal());
    }

    #[test]
    fn decoder_ignores_control_frames() {
        assert!(decode_frame(&json!({"type": "session.created", "session": {}}), false).is_empty());
        assert!(decode_frame(&json!({"type": "session.updated", "session": {}}), false).is_empty());
        // The commit acknowledgement (server-side conformance, 2026-09-13)
        // names the item, but carries no transcript: still a control frame.
        assert!(decode_frame(
            &json!({"type": "input_audio_buffer.committed", "event_id": "e", "item_id": "i1"}),
            false
        )
        .is_empty());
    }
}
