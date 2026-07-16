//! `myna-audio-adapter` captures audio from PipeWire/PulseAudio, converts it to a
//! target format, and delivers contiguous frames through stateless stream
//! primitives.
//!
//! # Quick start
//!
//! ```no_run
//! use myna_audio_adapter::{enumerate_nodes, open_stream, StreamConfig};
//!
//! let _nodes = enumerate_nodes().unwrap();
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

pub use backend::mock::MockBackend;

use crate::backend::AudioBackend;

/// Enumerate audio-producing input nodes with metadata.
pub fn enumerate_nodes() -> Result<Vec<InputNode>, Error> {
    backend::default_backend(BackendSelector::Auto)?.enumerate()
}

/// Ensure a capture stream is open on the selected node and return its handle.
///
/// Idempotent per node (FR-003): if the selected node already has an open
/// stream, the existing handle is returned and `config` does not alter the
/// already-open stream.
pub fn open_stream(config: &StreamConfig) -> Result<AudioStream, Error> {
    config.validate()?;
    let backend = backend::default_backend(config.backend)?;
    open_with(backend, config)
}

/// Open a stream using an explicitly provided backend (e.g. [`MockBackend`]
/// in tests). Goes through the same idempotent-open registry as
/// [`open_stream`], so contract behavior is identical.
pub fn open_stream_with_backend(
    config: &StreamConfig,
    backend: Box<dyn AudioBackend>,
) -> Result<AudioStream, Error> {
    config.validate()?;
    open_with(backend, config)
}

fn open_with(backend: Box<dyn AudioBackend>, config: &StreamConfig) -> Result<AudioStream, Error> {
    let nodes = backend.enumerate()?;
    let node = resolve_node(&nodes, &config.node)?;
    let target_format = config.target_format.clone();
    stream::open_or_existing(node, target_format.clone(), move || {
        let capacity_bytes = config.buffer_capacity_bytes();
        let (producer, consumer) =
            ring::AudioQueue::new(target_format, capacity_bytes).split();
        let backend_stream = backend.open(config.clone(), producer)?;
        Ok((consumer, backend_stream))
    })
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
