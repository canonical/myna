use crate::error::Error;
use crate::frame::{AudioFrame, StreamEvent};
use crate::preprocess::PreprocessStage;

/// RNNoise-based denoising stage.
pub struct DenoiseStage;

impl DenoiseStage {
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

impl PreprocessStage for DenoiseStage {
    fn process(&mut self, _frame: &mut AudioFrame) -> Result<Vec<StreamEvent>, Error> {
        // TODO: integrate nnnoiseless processing once the API is verified.
        Ok(Vec::new())
    }
}

impl Default for DenoiseStage {
    fn default() -> Self {
        Self::new().unwrap()
    }
}
