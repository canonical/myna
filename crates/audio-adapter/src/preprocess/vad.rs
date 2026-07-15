use crate::error::Error;
use crate::frame::{AudioFrame, StreamEvent};
use crate::preprocess::PreprocessStage;

/// Silero VAD stage that emits `VoiceActivity` events on transitions.
pub struct VadStage {
    last_speaking: Option<bool>,
}

impl VadStage {
    pub fn new() -> Result<Self, Error> {
        Ok(Self { last_speaking: None })
    }
}

impl PreprocessStage for VadStage {
    fn process(&mut self, frame: &mut AudioFrame) -> Result<Vec<StreamEvent>, Error> {
        // TODO: run Silero VAD inference once the onnxruntime API is verified.
        let speaking = false;
        let mut events = Vec::new();
        if self.last_speaking != Some(speaking) {
            self.last_speaking = Some(speaking);
            events.push(StreamEvent::VoiceActivity {
                speaking,
                at: frame.timestamp,
            });
        }
        Ok(events)
    }
}

impl Default for VadStage {
    fn default() -> Self {
        Self::new().unwrap()
    }
}
