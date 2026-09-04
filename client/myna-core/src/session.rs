//! Session configuration — the Rust mirror of Python `myna.core.session`.
//!
//! A flat config (IE115 reconciliation T37): no nested
//! `session.audio.input.transcription` envelope, no `turn_detection` (turn
//! detection is client-driven — the client signals end-of-audio explicitly, not
//! server VAD). Audio format travels with the config so the service can validate
//! it against its advertised capabilities and reject mismatches rather than
//! resample. Every field is serialized (including `null`s) to match the Python
//! `session_config_to_wire` (`asdict`) shape exactly.

use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default)]
    pub audio_format: AudioFormat,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub output_language: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// `"word" | "segment" | None` — request timestamped segments.
    #[serde(default)]
    pub timestamp_granularity: Option<String>,
}

impl SessionConfig {
    /// Encode to the wire object (Python `session_config_to_wire`).
    pub fn to_wire(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("session config is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn golden(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn default_config_wire_shape() {
        assert_eq!(
            SessionConfig::default().to_wire(),
            golden(
                r#"{"audio_format": {"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2},
                    "language": null, "output_language": null, "prompt": null,
                    "timestamp_granularity": null}"#
            )
        );
    }

    #[test]
    fn config_with_language_and_prompt() {
        let cfg = SessionConfig {
            language: Some("en".into()),
            prompt: Some("hello".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.to_wire(),
            golden(
                r#"{"audio_format": {"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2},
                    "language": "en", "output_language": null, "prompt": "hello",
                    "timestamp_granularity": null}"#
            )
        );
    }

    #[test]
    fn round_trip() {
        let cfg = SessionConfig {
            language: Some("fr".into()),
            ..Default::default()
        };
        let decoded: SessionConfig = serde_json::from_value(cfg.to_wire()).unwrap();
        assert_eq!(decoded, cfg);
    }
}
