use crate::format::AudioFormat;
use crate::node::NodeId;
use std::time::Duration;

/// Selector for the input node to capture from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NodeSelector {
    /// Use the system's current default input node.
    #[default]
    Default,
    /// Select by node id.
    ById(NodeId),
    /// Select by node name.
    ByName(String),
}

/// Selector for the audio backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendSelector {
    /// Automatically probe PipeWire first, then PulseAudio.
    #[default]
    Auto,
    /// Use the native PipeWire backend.
    PipeWire,
    /// Use the PulseAudio fallback backend.
    Pulse,
}

/// Configuration for optional preprocessing stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreprocessConfig {
    /// Enable noise suppression (RNNoise).
    pub denoise: bool,
    /// Enable voice activity detection (Silero VAD).
    pub vad: bool,
    /// Enable dereverberation (reserved for future implementation).
    pub deverb: bool,
}

/// Consumer-supplied settings for opening a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// Which node to capture from.
    pub node: NodeSelector,
    /// Target sample rate, format, and channel layout.
    pub target_format: AudioFormat,
    /// Maximum duration of the rolling buffer.
    pub max_buffer_duration: Duration,
    /// Preprocessing options.
    pub preprocess: PreprocessConfig,
    /// Backend override.
    pub backend: BackendSelector,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            node: NodeSelector::Default,
            target_format: AudioFormat::default_target(),
            max_buffer_duration: Duration::from_secs(10),
            preprocess: PreprocessConfig::default(),
            backend: BackendSelector::Auto,
        }
    }
}

impl StreamConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), crate::Error> {
        if self.target_format.sample_rate < 8_000 || self.target_format.sample_rate > 192_000 {
            return Err(crate::Error::UnsupportedFormat(format!(
                "sample_rate {} out of supported range",
                self.target_format.sample_rate
            )));
        }
        if self.target_format.channels == 0 {
            return Err(crate::Error::UnsupportedFormat(
                "channels must be >= 1".into(),
            ));
        }
        if self.max_buffer_duration.as_secs() == 0 && self.max_buffer_duration.subsec_nanos() == 0
        {
            return Err(crate::Error::UnsupportedFormat(
                "max_buffer_duration must be > 0".into(),
            ));
        }
        Ok(())
    }

    /// Byte capacity for the ring buffer derived from `max_buffer_duration`.
    pub fn buffer_capacity_bytes(&self) -> usize {
        self.target_format.bytes_for_duration(self.max_buffer_duration)
    }
}
