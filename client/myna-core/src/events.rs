//! Transcript event vocabulary and its JSON wire codec — the Rust mirror of
//! Python `myna.core.events`.
//!
//! The wire encoding is a transport-agnostic object shape,
//! `{"event": <type>, "data": {…}}`, so a transport only frames these objects.
//! The vocabulary is **additive** (aligned 2026-07-06 with the IE115
//! direction): [`TranscriptionEvent::from_wire`] returns `Ok(None)` for an
//! unknown event type and callers skip it, so a new server-side event does not
//! break deployed clients. [`crate::protocol`] version bumps are reserved for
//! semantic changes to existing events/shapes.
//!
//! Semantics (unchanged from the Python contract):
//! - [`TranscriptionEvent::Progress`] — liveness; never committed text. `phase`
//!   is [`PHASE_PREPARING`] (model loading), [`PHASE_READY`] (resident, accept
//!   gate open, nothing decoding yet), or [`PHASE_TRANSCRIBING`] (default).
//!   These three map onto the IE115 `STATUS` liveness `state`.
//! - [`TranscriptionEvent::Final`] — stable committed text for a segment; never
//!   retracted.
//! - [`TranscriptionEvent::Done`] — terminal; full transcript.
//! - [`TranscriptionEvent::Error`] — terminal; stable `code` + human `message`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// `progress.phase`: model is loading; the client should show "loading…".
pub const PHASE_PREPARING: &str = "preparing";
/// `progress.phase`: model resident, accept-gate open, nothing decoding yet.
/// The client may begin sending audio (T42 — the residency gate signal).
pub const PHASE_READY: &str = "ready";
/// `progress.phase`: audio is being processed (the default).
pub const PHASE_TRANSCRIBING: &str = "transcribing";

pub const EVENT_PROGRESS: &str = "transcription.progress";
pub const EVENT_FINAL: &str = "transcription.final";
pub const EVENT_DONE: &str = "transcription.done";
pub const EVENT_ERROR: &str = "transcription.error";

/// Disposition enum for streaming mode (T05, feature 007-streaming-mode)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    /// Text is final, append-only, safe to inject into text field
    Committed,
    /// Text is provisional, may be revised or superseded
    Unstable,
}

impl Default for Disposition {
    fn default() -> Self {
        Self::Committed // Backward-compatible default
    }
}

/// Streaming mode selector (T05, feature 007-streaming-mode)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamingMode {
    /// Force streaming regardless of tier (user accepts potential latency)
    Streaming,
    /// Force batch regardless of tier (user prefers all-at-once)
    Batch,
    /// Resolve to streaming or batch based on tier assessment (default)
    Auto,
}

impl Default for StreamingMode {
    fn default() -> Self {
        Self::Auto
    }
}

fn default_phase() -> String {
    PHASE_TRANSCRIBING.to_string()
}

fn default_code() -> String {
    "internal".to_string()
}

/// Timestamped transcript segment, relative to session start (seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub score: Option<f64>,
}

/// `transcription.progress` payload — lightweight liveness. `snippet` is
/// unstable UI-animation text with no accuracy or retraction guarantee.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub snippet: Option<String>,
    #[serde(default = "default_phase")]
    pub phase: String,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            snippet: None,
            phase: default_phase(),
        }
    }
}

impl Progress {
    /// A bare liveness ping in `phase`, no snippet.
    pub fn phase(phase: impl Into<String>) -> Self {
        Self {
            snippet: None,
            phase: phase.into(),
        }
    }
}

/// `transcription.final` / `transcription.done` payload — committed text plus
/// optional timestamped segments.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionFinal {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub disposition: Disposition,
    #[serde(default)]
    pub segment_index: Option<u32>,
}

/// `transcription.error` payload — stable machine-readable `code`, human
/// `message`. Terminal for the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    #[serde(default = "default_code")]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

impl Default for ErrorData {
    fn default() -> Self {
        Self {
            code: default_code(),
            message: String::new(),
        }
    }
}

/// A transcript event. `Final` and `Done` share the [`TranscriptionFinal`]
/// payload shape (matching the Python `TranscriptionFinal` / `TranscriptionDone`
/// `{text, segments}`); the variant distinguishes committed-segment from
/// session-terminal.
#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptionEvent {
    Progress(Progress),
    Final(TranscriptionFinal),
    Done(TranscriptionFinal),
    Error(ErrorData),
}

/// Failure decoding a wire event.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("frame is not a transcript event (missing string \"event\" key)")]
    NotAnEvent,
    #[error("malformed event data: {0}")]
    BadData(#[from] serde_json::Error),
}

impl TranscriptionEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            TranscriptionEvent::Progress(_) => EVENT_PROGRESS,
            TranscriptionEvent::Final(_) => EVENT_FINAL,
            TranscriptionEvent::Done(_) => EVENT_DONE,
            TranscriptionEvent::Error(_) => EVENT_ERROR,
        }
    }

    /// Terminal events end a session: exactly one is emitted last.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TranscriptionEvent::Done(_) | TranscriptionEvent::Error(_)
        )
    }

    /// Encode to the `{"event": …, "data": {…}}` wire object (Python
    /// `event_to_wire`).
    pub fn to_wire(&self) -> Value {
        let data = match self {
            TranscriptionEvent::Progress(p) => serde_json::to_value(p),
            TranscriptionEvent::Final(f) | TranscriptionEvent::Done(f) => serde_json::to_value(f),
            TranscriptionEvent::Error(e) => serde_json::to_value(e),
        }
        .expect("event payloads are always serializable");
        json!({ "event": self.event_type(), "data": data })
    }

    /// Decode from the wire object (Python `event_from_wire`). `Ok(None)` for an
    /// unknown event type — additive compat, the caller skips it.
    pub fn from_wire(wire: &Value) -> Result<Option<Self>, WireError> {
        let event = wire
            .get("event")
            .and_then(Value::as_str)
            .ok_or(WireError::NotAnEvent)?;
        // Python: `dict(wire.get("data") or {})` — a missing/null data is {}.
        let data = match wire.get("data") {
            Some(Value::Null) | None => json!({}),
            Some(v) => v.clone(),
        };
        Ok(Some(match event {
            EVENT_PROGRESS => TranscriptionEvent::Progress(serde_json::from_value(data)?),
            EVENT_FINAL => TranscriptionEvent::Final(serde_json::from_value(data)?),
            EVENT_DONE => TranscriptionEvent::Done(serde_json::from_value(data)?),
            EVENT_ERROR => TranscriptionEvent::Error(serde_json::from_value(data)?),
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert `event.to_wire()` equals a golden frame captured from Python
    /// `myna.core.events.event_to_wire` (compared as JSON values, so key order
    /// and whitespace don't matter — both ends parse JSON).
    fn assert_wire(event: &TranscriptionEvent, golden: &str) {
        let expected: Value = serde_json::from_str(golden).unwrap();
        assert_eq!(event.to_wire(), expected, "wire mismatch vs Python golden");
        // Round-trip: decoding our own output reproduces the event.
        assert_eq!(
            &TranscriptionEvent::from_wire(&event.to_wire())
                .unwrap()
                .unwrap(),
            event
        );
        // And decoding the Python golden reproduces it too.
        assert_eq!(
            &TranscriptionEvent::from_wire(&expected).unwrap().unwrap(),
            event
        );
    }

    #[test]
    fn progress_ready() {
        assert_wire(
            &TranscriptionEvent::Progress(Progress::phase(PHASE_READY)),
            r#"{"event": "transcription.progress", "data": {"snippet": null, "phase": "ready"}}"#,
        );
    }

    #[test]
    fn progress_with_snippet_preparing() {
        assert_wire(
            &TranscriptionEvent::Progress(Progress {
                snippet: Some("the quick".into()),
                phase: PHASE_PREPARING.into(),
            }),
            r#"{"event": "transcription.progress", "data": {"snippet": "the quick", "phase": "preparing"}}"#,
        );
    }

    #[test]
    fn progress_default_is_transcribing() {
        assert_wire(
            &TranscriptionEvent::Progress(Progress::default()),
            r#"{"event": "transcription.progress", "data": {"snippet": null, "phase": "transcribing"}}"#,
        );
    }

    #[test]
    fn final_text_only() {
        assert_wire(
            &TranscriptionEvent::Final(TranscriptionFinal {
                text: "The quick brown fox".into(),
                segments: vec![],
                disposition: Disposition::default(),
                segment_index: None,
            }),
            r#"{"event": "transcription.final", "data": {"text": "The quick brown fox", "segments": [], "disposition": "committed", "segment_index": null}}"#,
        );
    }

    #[test]
    fn final_with_segment() {
        assert_wire(
            &TranscriptionEvent::Final(TranscriptionFinal {
                text: "hi".into(),
                segments: vec![Segment {
                    start: 0.0,
                    end: 1.0,
                    text: "hi".into(),
                    score: None,
                }],
                disposition: Disposition::default(),
                segment_index: None,
            }),
            r#"{"event": "transcription.final", "data": {"text": "hi", "segments": [{"start": 0.0, "end": 1.0, "text": "hi", "score": null}], "disposition": "committed", "segment_index": null}}"#,
        );
    }

    #[test]
    fn done_full_transcript() {
        assert_wire(
            &TranscriptionEvent::Done(TranscriptionFinal {
                text: "full text".into(),
                segments: vec![],
                disposition: Disposition::default(),
                segment_index: None,
            }),
            r#"{"event": "transcription.done", "data": {"text": "full text", "segments": [], "disposition": "committed", "segment_index": null}}"#,
        );
    }

    #[test]
    fn error_frame() {
        assert_wire(
            &TranscriptionEvent::Error(ErrorData {
                code: "internal".into(),
                message: "boom".into(),
            }),
            r#"{"event": "transcription.error", "data": {"code": "internal", "message": "boom"}}"#,
        );
    }

    #[test]
    fn terminal_classification() {
        assert!(!TranscriptionEvent::Progress(Progress::default()).is_terminal());
        assert!(!TranscriptionEvent::Final(TranscriptionFinal::default()).is_terminal());
        assert!(TranscriptionEvent::Done(TranscriptionFinal::default()).is_terminal());
        assert!(TranscriptionEvent::Error(ErrorData::default()).is_terminal());
    }

    #[test]
    fn unknown_event_type_is_ignored_not_rejected() {
        // Additive compat: a new server-side event must not break this client.
        let wire = json!({"event": "transcription.partial", "data": {}});
        assert!(matches!(TranscriptionEvent::from_wire(&wire), Ok(None)));
    }

    #[test]
    fn missing_event_key_rejected() {
        let wire = json!({"type": "session.created", "protocol_version": "1"});
        assert!(matches!(
            TranscriptionEvent::from_wire(&wire),
            Err(WireError::NotAnEvent)
        ));
    }

    #[test]
    fn missing_data_defaults_like_python() {
        // Python `event_from_wire` treats absent/null data as {} → defaults.
        let wire = json!({"event": "transcription.progress"});
        assert_eq!(
            TranscriptionEvent::from_wire(&wire).unwrap().unwrap(),
            TranscriptionEvent::Progress(Progress::default())
        );
    }

    // T009: Disposition encoding/decoding tests (feature 007-streaming-mode)
    #[test]
    fn final_with_disposition_committed() {
        assert_wire(
            &TranscriptionEvent::Final(TranscriptionFinal {
                text: "Hello world".into(),
                segments: vec![],
                disposition: Disposition::Committed,
                segment_index: Some(0),
            }),
            r#"{"event": "transcription.final", "data": {"text": "Hello world", "segments": [], "disposition": "committed", "segment_index": 0}}"#,
        );
    }

    #[test]
    fn final_with_disposition_unstable() {
        assert_wire(
            &TranscriptionEvent::Final(TranscriptionFinal {
                text: "Hello wor".into(),
                segments: vec![],
                disposition: Disposition::Unstable,
                segment_index: None,
            }),
            r#"{"event": "transcription.final", "data": {"text": "Hello wor", "segments": [], "disposition": "unstable", "segment_index": null}}"#,
        );
    }

    #[test]
    fn disposition_default_is_committed() {
        // T010: Backward-compat test - absent disposition defaults to committed
        let wire =
            json!({"event": "transcription.final", "data": {"text": "test", "segments": []}});
        let event = TranscriptionEvent::from_wire(&wire).unwrap().unwrap();
        if let TranscriptionEvent::Final(final_event) = event {
            assert_eq!(final_event.disposition, Disposition::Committed);
            assert_eq!(final_event.segment_index, None);
        } else {
            panic!("Expected TranscriptionEvent::Final");
        }
    }
}
