//! Optional audio preprocessing stages (denoise, VAD, deverb).

#[cfg(feature = "denoise")]
pub mod denoise;

#[cfg(feature = "vad")]
pub mod vad;

use crate::error::Error;
use crate::frame::{AudioFrame, StreamEvent};

/// A single preprocessing stage.
pub trait PreprocessStage: Send {
    /// Process a frame in place, returning any events emitted.
    fn process(&mut self, frame: &mut AudioFrame) -> Result<Vec<StreamEvent>, Error>;
}

/// A chain of preprocessing stages.
pub struct StageChain {
    stages: Vec<Box<dyn PreprocessStage>>,
}

impl StageChain {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add(&mut self, stage: Box<dyn PreprocessStage>) {
        self.stages.push(stage);
    }

    pub fn process(&mut self, frame: &mut AudioFrame) -> Result<Vec<StreamEvent>, Error> {
        let mut events = Vec::new();
        for stage in &mut self.stages {
            events.extend(stage.process(frame)?);
        }
        Ok(events)
    }
}

impl Default for StageChain {
    fn default() -> Self {
        Self::new()
    }
}
