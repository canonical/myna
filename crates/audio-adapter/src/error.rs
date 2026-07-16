use thiserror::Error;

/// Errors returned by the audio adapter.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No matching audio input node was found.
    #[error("no audio input node found")]
    NoDevice,

    /// The audio server denied capture permission.
    #[error("permission denied capturing audio")]
    PermissionDenied,

    /// The source format cannot be converted to the requested target format.
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),

    /// The selected input node was disconnected or otherwise lost.
    #[error("audio input device lost")]
    DeviceLost,

    /// A backend-specific error occurred.
    #[error("audio backend error: {0}")]
    Backend(String),
}
