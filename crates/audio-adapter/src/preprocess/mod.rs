//! Optional audio preprocessing (denoise, VAD, deverb).
//!
//! Not implemented yet: `StreamConfig::validate` rejects enabled preprocess
//! flags so a consumer can never silently believe preprocessing is active
//! (FR-010/FR-011 pending). The trait below is the integration point future
//! stages (RNNoise denoise behind feature `denoise`, Silero VAD behind
//! feature `vad`, and an eventual deverb stage — deferred per FR-011 MAY)
//! will implement; adding stages is a non-breaking change.

use crate::error::Error;
use crate::frame::{AudioFrame, StreamEvent};

/// A single preprocessing stage, applied to frames before delivery.
pub trait PreprocessStage: Send {
    /// Process a frame in place, returning any events emitted (e.g.
    /// `StreamEvent::VoiceActivity` transitions from a VAD stage).
    fn process(&mut self, frame: &mut AudioFrame) -> Result<Vec<StreamEvent>, Error>;
}
