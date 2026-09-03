//! Service capabilities — what a backend can do, for client discovery (T24).
//! Rust mirror of Python `myna.core.capabilities`.
//!
//! A running server serves one model, so `models` reports the active one
//! rather than everything installable (model *selection* is out of band, via
//! the IE108/modelctl CLI).

use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The active model id(s) the server serves (informational).
    #[serde(default)]
    pub models: Vec<String>,
    /// BCP-47-ish input language tags, or `["*"]` for "any language the
    /// multilingual model handles".
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    /// The PCM formats the service accepts.
    #[serde(default = "default_input_formats")]
    pub input_formats: Vec<AudioFormat>,
    /// The model emits punctuation/capitalisation natively.
    #[serde(default)]
    pub punctuation: bool,
    /// The service can output a language different from the input.
    #[serde(default)]
    pub translation: bool,
}

fn default_languages() -> Vec<String> {
    vec!["*".into()]
}

fn default_input_formats() -> Vec<AudioFormat> {
    vec![AudioFormat::default()]
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            languages: default_languages(),
            input_formats: default_input_formats(),
            punctuation: false,
            translation: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn golden(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn wire(v: &impl Serialize) -> Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn defaults_are_conservative() {
        let caps = Capabilities::default();
        assert_eq!(caps.languages, vec!["*".to_string()]);
        assert_eq!(caps.input_formats, vec![AudioFormat::default()]);
        assert!(!caps.punctuation);
        assert!(!caps.translation);
    }

    #[test]
    fn wire_shape_matches_python() {
        let caps = Capabilities {
            models: vec!["whisper-small".into()],
            languages: vec!["*".into()],
            input_formats: vec![AudioFormat::default()],
            punctuation: true,
            translation: false,
        };
        assert_eq!(
            wire(&caps),
            golden(
                r#"{"models": ["whisper-small"], "languages": ["*"],
                    "input_formats": [{"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2}],
                    "punctuation": true, "translation": false}"#
            )
        );
    }

    #[test]
    fn round_trip() {
        let caps = Capabilities {
            models: vec!["parakeet-tdt".into()],
            ..Capabilities::default()
        };
        let decoded: Capabilities = serde_json::from_value(wire(&caps)).unwrap();
        assert_eq!(decoded, caps);
    }
}
