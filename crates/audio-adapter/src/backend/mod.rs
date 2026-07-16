use crate::config::{BackendSelector, StreamConfig};
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
    fn open(
        &self,
        config: StreamConfig,
        producer: QueueProducer,
    ) -> Result<Box<dyn BackendStream>, Error>;
}

/// Handle to a running backend capture stream.
pub trait BackendStream: Send {
    /// Stop capture and release resources.
    fn close(&mut self) -> Result<(), Error>;
}

/// Resolve the backend the consumer selected (FR-021 launch-time selection):
/// `Auto` probes PipeWire first, then PulseAudio. There is no silent fallback
/// to any other implementation — if no real audio server is reachable, this
/// returns an error (mock capture is only ever available by explicit
/// injection via `open_stream_with_backend`).
pub fn default_backend(selector: BackendSelector) -> Result<Box<dyn AudioBackend>, Error> {
    match selector {
        BackendSelector::Auto => {
            #[cfg(feature = "pipewire")]
            if let Ok(backend) = pipewire::PipeWireBackend::new() {
                return Ok(Box::new(backend));
            }
            #[cfg(feature = "pulse")]
            if let Ok(backend) = pulse::PulseAudioBackend::new() {
                return Ok(Box::new(backend));
            }
            Err(Error::Backend("no audio backend available".into()))
        }
        BackendSelector::PipeWire => {
            #[cfg(feature = "pipewire")]
            {
                Ok(Box::new(pipewire::PipeWireBackend::new()?))
            }
            #[cfg(not(feature = "pipewire"))]
            {
                Err(Error::Backend(
                    "PipeWire backend not compiled in (feature \"pipewire\")".into(),
                ))
            }
        }
        BackendSelector::Pulse => {
            #[cfg(feature = "pulse")]
            {
                Ok(Box::new(pulse::PulseAudioBackend::new()?))
            }
            #[cfg(not(feature = "pulse"))]
            {
                Err(Error::Backend(
                    "PulseAudio backend not compiled in (feature \"pulse\")".into(),
                ))
            }
        }
    }
}
