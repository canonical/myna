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
    /// Words/phrases (names, jargon) a backend biases recognition toward.
    /// Empty means no biasing.
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl SessionConfig {
    /// Encode to the wire object (Python `session_config_to_wire`).
    pub fn to_wire(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("session config is always serializable")
    }
}

/// Local-dictionary limits (GitHub issue #1), mirroring the server's
/// `KEYWORDS_MAX_ENTRIES` / `KEYWORDS_MAX_ENTRY_CHARS`
/// (`server/src/myna/core/session.py`). The server rejects a list past these
/// with `invalid_request_error`; [`normalize_keywords`] keeps a client from
/// ever sending one.
pub const KEYWORDS_MAX_ENTRIES: usize = 100;
pub const KEYWORDS_MAX_ENTRY_CHARS: usize = 64;

/// Trim, drop empties, dedupe and cap a raw vocabulary list so it can never
/// trip the server's own limits at `session.update`.
///
/// Unlike the server's `normalize_keywords`, this never rejects: a setting
/// that has drifted past the limits (a hand-edited keyfile, a future settings
/// UI bug) is silently clamped rather than failing every dictation. Entries
/// are counted and cut in `char`s, matching Python's `len(str)`.
pub fn normalize_keywords(entries: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let capped: String = trimmed.chars().take(KEYWORDS_MAX_ENTRY_CHARS).collect();
        if seen.insert(capped.clone()) {
            out.push(capped);
            if out.len() == KEYWORDS_MAX_ENTRIES {
                break;
            }
        }
    }
    out
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
                    "timestamp_granularity": null, "keywords": []}"#
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
                    "timestamp_granularity": null, "keywords": []}"#
            )
        );
    }

    #[test]
    fn config_with_keywords() {
        let cfg = SessionConfig {
            keywords: vec!["kernel".into(), "Siobhan".into()],
            ..Default::default()
        };
        assert_eq!(
            cfg.to_wire(),
            golden(
                r#"{"audio_format": {"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2},
                    "language": null, "output_language": null, "prompt": null,
                    "timestamp_granularity": null, "keywords": ["kernel", "Siobhan"]}"#
            )
        );
    }

    #[test]
    fn round_trip() {
        let cfg = SessionConfig {
            language: Some("fr".into()),
            keywords: vec!["kernel".into()],
            ..Default::default()
        };
        let decoded: SessionConfig = serde_json::from_value(cfg.to_wire()).unwrap();
        assert_eq!(decoded, cfg);
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn normalize_keywords_trims_drops_empties_and_dedupes() {
        let input = strings(&[" kernel ", "", "   ", "kernel", "Siobhan"]);
        assert_eq!(normalize_keywords(&input), strings(&["kernel", "Siobhan"]));
    }

    #[test]
    fn normalize_keywords_caps_entry_count() {
        let input: Vec<String> = (0..KEYWORDS_MAX_ENTRIES + 50)
            .map(|i| format!("word{i}"))
            .collect();
        let out = normalize_keywords(&input);
        assert_eq!(out.len(), KEYWORDS_MAX_ENTRIES);
        assert_eq!(out[0], "word0");
        assert_eq!(
            out[KEYWORDS_MAX_ENTRIES - 1],
            format!("word{}", KEYWORDS_MAX_ENTRIES - 1)
        );
    }

    #[test]
    fn normalize_keywords_caps_entry_length_in_chars_not_bytes() {
        // "é" is one char, two UTF-8 bytes: the cap must count chars, like Python's len(str).
        let long: String = "é".repeat(KEYWORDS_MAX_ENTRY_CHARS + 10);
        let out = normalize_keywords(&[long]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chars().count(), KEYWORDS_MAX_ENTRY_CHARS);
    }

    #[test]
    fn normalize_keywords_of_an_empty_list_is_empty() {
        assert!(normalize_keywords(&[]).is_empty());
    }
}
