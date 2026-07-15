use crate::config::StreamConfig;
use crate::error::Error;
use crate::node::InputNode;
use crate::ring::QueueProducer;

pub mod mock;

#[cfg(feature = "pipewire")]
pub mod pipewire;

#[cfg(feature = "pulse")]
pub mod pulse;

/// Abstraction over a PipeWire/PulseAudio-style audio server.
pub trait AudioBackend: Send + Sync {
    /// Enumerate available input nodes.
    fn enumerate(&self) -> Result<Vec<InputNode>, Error>;

    /// Open a capture stream on the selected node, feeding frames into `producer`.
    fn open(&self, config: StreamConfig, producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error>;
}

/// Handle to a running backend capture stream.
pub trait BackendStream: Send {
    /// Stop capture and release resources.
    fn close(&mut self) -> Result<(), Error>;
}

/// Auto-probe backend: PipeWire first, then PulseAudio.
pub fn default_backend() -> Result<Box<dyn AudioBackend>, Error> {
    #[cfg(feature = "pipewire")]
    if let Ok(backend) = pipewire::PipeWireBackend::new() {
        return Ok(Box::new(backend));
    }

    #[cfg(feature = "pulse")]
    if let Ok(backend) = pulse::PulseBackend::new() {
        return Ok(Box::new(backend));
    }

    #[cfg(any(test, feature = "test-util"))]
    {
        Ok(Box::new(mock::MockBackend::new()))
    }

    #[cfg(not(any(test, feature = "test-util")))]
    {
        Err(Error::Backend("no audio backend available".into()))
    }
}
