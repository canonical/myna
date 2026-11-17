//! The transcription mode in force: the user's explicit choice, else the
//! active backend's own default.
//!
//! One resolver, so the daemon and Myna Settings cannot answer differently.
//! "No choice" is the absence of a user value in the store, not a third enum
//! value: the dropdown stays two options, and whatever the user picks wins.

use std::fmt;

use crate::StreamingMode;

/// Where the mode in force came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeSource {
    /// The user set `streaming-mode`.
    User,
    /// No user value; the backend said whether it streams.
    Backend,
    /// No user value and the backend did not say: the schema default.
    Unknown,
}

/// The resolved mode and why it is that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveMode {
    pub mode: StreamingMode,
    pub source: ModeSource,
}

/// `user` is the store's user value (`None` when unset); `backend_streams` is
/// the backend's `Capabilities.streaming`, `None` when unknown (an older
/// server, a failed query, or no backend asked yet).
pub fn effective_mode(user: Option<StreamingMode>, backend_streams: Option<bool>) -> EffectiveMode {
    match (user, backend_streams) {
        (Some(mode), _) => EffectiveMode {
            mode,
            source: ModeSource::User,
        },
        (None, Some(streams)) => EffectiveMode {
            mode: if streams {
                StreamingMode::Streaming
            } else {
                StreamingMode::Batch
            },
            source: ModeSource::Backend,
        },
        (None, None) => EffectiveMode {
            mode: StreamingMode::default(),
            source: ModeSource::Unknown,
        },
    }
}

/// Whether a backend snap streams when nobody set its engine's `streaming`,
/// for Settings, which has no socket to ask. Its installer's default, so it
/// matches `Capabilities.streaming` until someone overrides the engine by
/// hand. A snap Myna does not know is taken as batch, like the fake backend.
pub fn streams_by_default(snap_name: &str) -> bool {
    crate::language::ModelFamily::from_snap_name(snap_name)
        .is_some_and(crate::language::ModelFamily::streams_by_default)
}

impl fmt::Display for EffectiveMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = self.mode;
        match self.source {
            ModeSource::User => write!(f, "streaming-mode {mode:?}, set by the user"),
            ModeSource::Backend => write!(f, "streaming-mode {mode:?}, the backend's default"),
            ModeSource::Unknown => write!(
                f,
                "streaming-mode {mode:?}, the schema default while the backend's is unknown"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [StreamingMode; 2] = [StreamingMode::Streaming, StreamingMode::Batch];

    #[test]
    fn an_explicit_choice_always_wins() {
        for user in MODES {
            for backend in [Some(true), Some(false), None] {
                assert_eq!(
                    effective_mode(Some(user), backend),
                    EffectiveMode {
                        mode: user,
                        source: ModeSource::User
                    },
                    "{user:?} vs backend {backend:?}"
                );
            }
        }
    }

    #[test]
    fn without_a_choice_the_backend_decides() {
        assert_eq!(
            effective_mode(None, Some(true)),
            EffectiveMode {
                mode: StreamingMode::Streaming,
                source: ModeSource::Backend
            }
        );
        assert_eq!(
            effective_mode(None, Some(false)),
            EffectiveMode {
                mode: StreamingMode::Batch,
                source: ModeSource::Backend
            }
        );
    }

    #[test]
    fn an_unknown_backend_reads_the_schema_default() {
        assert_eq!(
            effective_mode(None, None),
            EffectiveMode {
                mode: StreamingMode::Streaming,
                source: ModeSource::Unknown
            }
        );
    }

    #[test]
    fn parakeet_and_indicconformer_stream_out_of_the_box() {
        assert!(streams_by_default("myna-parakeet"));
        assert!(streams_by_default("myna-indicconformer"));
        assert!(!streams_by_default("myna-whisper"));
        assert!(!streams_by_default("myna-funasr"));
        assert!(!streams_by_default("myna-fake-backend"));
    }

    #[test]
    fn the_reason_names_the_source() {
        assert_eq!(
            effective_mode(Some(StreamingMode::Batch), Some(true)).to_string(),
            "streaming-mode Batch, set by the user"
        );
        assert_eq!(
            effective_mode(None, Some(false)).to_string(),
            "streaming-mode Batch, the backend's default"
        );
        assert_eq!(
            effective_mode(None, None).to_string(),
            "streaming-mode Streaming, the schema default while the backend's is unknown"
        );
    }
}
