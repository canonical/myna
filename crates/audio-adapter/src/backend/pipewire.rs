use crate::backend::{AudioBackend, BackendStream};
use crate::config::StreamConfig;
use crate::error::Error;
use crate::node::InputNode;
use crate::ring::QueueProducer;

/// Native PipeWire backend.
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> Result<Self, Error> {
        // TODO: probe PipeWire daemon availability.
        Err(Error::Backend(
            "PipeWire backend not yet implemented".into(),
        ))
    }
}

impl AudioBackend for PipeWireBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        todo!("PipeWire enumerate")
    }

    fn open(&self, _config: StreamConfig, _producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error> {
        todo!("PipeWire open")
    }
}

struct PipeWireStream;

impl BackendStream for PipeWireStream {
    fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
