//! Handshake / control frames — the JSON messages that carry a `"type"` key,
//! as distinct from transcript events (which carry `"event"`). Rust mirror of
//! the control side of Python `myna.core.transport_ws`.
//!
//! [`ClientControl`] and [`ServerControl`] are internally-tagged serde enums, so
//! the `"type"` discriminant is emitted first and the shapes match the Python
//! frames exactly (e.g. `{"type": "session.finish"}`,
//! `{"type": "session.start", "protocol_version": …, "config": …}`).

use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;
use crate::session::SessionConfig;

/// Client → server control frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientControl {
    /// Open a session: declares the protocol version and the session config.
    #[serde(rename = "session.start")]
    SessionStart {
        protocol_version: String,
        config: SessionConfig,
    },
    /// End of audio (hotkey released). Closing the socket instead *aborts*.
    #[serde(rename = "session.finish")]
    SessionFinish,
    /// Pre-session discovery: ask for the service `Capabilities`.
    #[serde(rename = "capabilities.query")]
    CapabilitiesQuery,
}

/// Server → client control frames. (Transcript events use the `"event"` shape in
/// [`crate::events`]; the version-mismatch failure is a terminal
/// `transcription.error` event, not a control frame.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerControl {
    /// Positive handshake ack, echoing the version the server will speak.
    #[serde(rename = "session.created")]
    SessionCreated { protocol_version: String },
    /// Reply to `capabilities.query`: what the backend can do (T24).
    #[serde(rename = "capabilities")]
    Capabilities { data: Capabilities },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;
    use serde_json::Value;

    fn golden(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn wire(v: &impl Serialize) -> Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn session_start_matches_python() {
        let frame = ClientControl::SessionStart {
            protocol_version: PROTOCOL_VERSION.into(),
            config: SessionConfig::default(),
        };
        assert_eq!(
            wire(&frame),
            golden(
                r#"{"type": "session.start", "protocol_version": "1",
                    "config": {"audio_format": {"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2},
                               "language": null, "output_language": null, "prompt": null,
                               "timestamp_granularity": null}}"#
            )
        );
    }

    #[test]
    fn session_finish_matches_python() {
        assert_eq!(
            wire(&ClientControl::SessionFinish),
            golden(r#"{"type": "session.finish"}"#)
        );
    }

    #[test]
    fn capabilities_query_matches_python() {
        assert_eq!(
            wire(&ClientControl::CapabilitiesQuery),
            golden(r#"{"type": "capabilities.query"}"#)
        );
    }

    #[test]
    fn session_created_matches_python() {
        let frame = ServerControl::SessionCreated {
            protocol_version: PROTOCOL_VERSION.into(),
        };
        assert_eq!(
            wire(&frame),
            golden(r#"{"type": "session.created", "protocol_version": "1"}"#)
        );
    }

    #[test]
    fn capabilities_reply_matches_python() {
        let frame = ServerControl::Capabilities {
            data: crate::capabilities::Capabilities {
                models: vec!["parakeet-tdt-0.6b-v2".into()],
                ..crate::capabilities::Capabilities::default()
            },
        };
        assert_eq!(
            wire(&frame),
            golden(
                r#"{"type": "capabilities", "data": {
                    "models": ["parakeet-tdt-0.6b-v2"], "languages": ["*"],
                    "input_formats": [{"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2}],
                    "punctuation": false, "translation": false}}"#
            )
        );
    }

    #[test]
    fn server_control_round_trips() {
        for frame in [
            ServerControl::SessionCreated {
                protocol_version: "1".into(),
            },
            ServerControl::Capabilities {
                data: crate::capabilities::Capabilities::default(),
            },
        ] {
            let decoded: ServerControl = serde_json::from_value(wire(&frame)).unwrap();
            assert_eq!(decoded, frame);
        }
    }

    #[test]
    fn client_control_round_trips() {
        for frame in [
            ClientControl::SessionStart {
                protocol_version: "1".into(),
                config: SessionConfig::default(),
            },
            ClientControl::SessionFinish,
            ClientControl::CapabilitiesQuery,
        ] {
            let decoded: ClientControl = serde_json::from_value(wire(&frame)).unwrap();
            assert_eq!(decoded, frame);
        }
    }
}
