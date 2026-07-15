use crate::backend::{AudioBackend, BackendStream};
use crate::config::StreamConfig;
use crate::error::Error;
use crate::node::InputNode;
use crate::ring::QueueProducer;

/// PulseAudio fallback backend.
pub struct PulseBackend;

impl PulseBackend {
    pub fn new() -> Result<Self, Error> {
        // TODO: probe PulseAudio daemon availability.
        Err(Error::Backend(
            "PulseAudio backend not yet implemented".into(),
        ))
    }
}

impl AudioBackend for PulseBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        todo!("PulseAudio enumerate")
    }

    fn open(&self, _config: StreamConfig, _producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error> {
        todo!("PulseAudio open")
    }
}

struct PulseStream;

impl BackendStream for PulseStream {
    fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
