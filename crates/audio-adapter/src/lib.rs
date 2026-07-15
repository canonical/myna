//! `myna-audio-adapter` captures audio from PipeWire/PulseAudio, converts it to a
//! target format, and delivers contiguous frames through stateless stream
//! primitives.
//!
//! # Quick start
//!
//! ```no_run
//! use myna_audio_adapter::{enumerate_nodes, open_stream, StreamConfig};
//!
//! let nodes = enumerate_nodes().unwrap();
//! let config = StreamConfig::default();
//! let mut stream = open_stream(&config).unwrap();
//! for item in stream.read_timeout(std::time::Duration::from_millis(100)).unwrap() {
//!     println!("{:?}", item);
//! }
//! ```

pub mod backend;
pub mod config;
pub mod convert;
pub mod error;
pub mod format;
pub mod frame;
pub mod node;
pub mod preprocess;
pub mod ring;
pub mod stream;

#[cfg(feature = "async")]
pub mod async_stream;

pub use config::{BackendSelector, NodeSelector, PreprocessConfig, StreamConfig};
pub use error::Error;
pub use format::{AudioFormat, SampleFormat};
pub use frame::{AudioFrame, StreamEvent, StreamItem};
pub use node::{InputNode, NodeId};
pub use stream::AudioStream;

#[cfg(any(test, feature = "test-util"))]
pub use backend::mock::MockBackend;

#[cfg(any(test, feature = "test-util"))]
use crate::backend::AudioBackend;

#[cfg(any(test, feature = "test-util"))]
/// Open a stream using an explicit backend. Useful for tests.
pub fn open_stream_with_backend(
    config: &StreamConfig,
    backend: Box<dyn AudioBackend>,
) -> Result<AudioStream, Error> {
    config.validate()?;
    let nodes = backend.enumerate()?;
    let node = resolve_node(&nodes, &config.node)?;
    // Test helper always creates a fresh stream to avoid cross-test registry contamination.
    let capacity_bytes = config.buffer_capacity_bytes();
    let (producer, consumer) = ring::AudioQueue::new(config.target_format.clone(), capacity_bytes).split();
    let backend_stream = backend.open(config.clone(), producer)?;
    Ok(AudioStream::new(consumer, backend_stream, node, config.target_format.clone()))
}

/// Enumerate audio-producing input nodes with metadata.
pub fn enumerate_nodes() -> Result<Vec<InputNode>, Error> {
    backend::default_backend()?.enumerate()
}

/// Ensure a capture stream is open on the selected node and return its handle.
///
/// If the selected node already has an open stream, the existing handle is
/// returned and `config` is ignored for the already-open stream.
pub fn open_stream(config: &StreamConfig) -> Result<AudioStream, Error> {
    config.validate()?;

    let backend = backend::default_backend()?;
    let nodes = backend.enumerate()?;
    let node = resolve_node(&nodes, &config.node)?;

    // Idempotent ensure-open: return the existing handle if present (production only).
    #[cfg(not(any(test, feature = "test-util")))]
    if let Some(stream) = stream::get_existing_stream(&node.id) {
        return Ok(stream);
    }

    let capacity_bytes = config.buffer_capacity_bytes();
    let (producer, consumer) = ring::AudioQueue::new(config.target_format.clone(), capacity_bytes).split();
    let backend_stream = backend.open(config.clone(), producer)?;
    Ok(AudioStream::new(consumer, backend_stream, node, config.target_format.clone()))
}



fn resolve_node(nodes: &[InputNode], selector: &NodeSelector) -> Result<InputNode, Error> {
    match selector {
        NodeSelector::Default => nodes
            .iter()
            .find(|n| n.is_default)
            .cloned()
            .or_else(|| nodes.first().cloned())
            .ok_or(Error::NoDevice),
        NodeSelector::ById(id) => nodes
            .iter()
            .find(|n| n.id == *id)
            .cloned()
            .ok_or(Error::NoDevice),
        NodeSelector::ByName(name) => nodes
            .iter()
            .find(|n| n.name == *name)
            .cloned()
            .ok_or(Error::NoDevice),
    }
}


