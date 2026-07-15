use crate::backend::BackendStream;
use crate::error::Error;
use crate::format::AudioFormat;
use crate::frame::StreamItem;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueConsumer;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use std::sync::LazyLock;

/// Global registry of currently open streams, keyed by node id.
/// Weak references allow streams to be closed when the last handle is dropped.
static OPEN_STREAMS: LazyLock<Mutex<std::collections::HashMap<NodeId, Weak<StreamInner>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

#[cfg(not(any(test, feature = "test-util")))]
pub(crate) fn get_existing_stream(node_id: &NodeId) -> Option<AudioStream> {
    let mut registry = OPEN_STREAMS.lock();
    // Clean up dead entries while we search.
    registry.retain(|_, weak| weak.strong_count() > 0);
    registry.get(node_id).and_then(|weak| weak.upgrade()).map(AudioStream::from_inner)
}

#[cfg(not(any(test, feature = "test-util")))]
pub(crate) fn register_stream(node_id: NodeId, inner: Weak<StreamInner>) {
    OPEN_STREAMS.lock().insert(node_id, inner);
}

pub(crate) fn unregister_stream(node_id: &NodeId) {
    OPEN_STREAMS.lock().remove(node_id);
}

pub(crate) struct StreamInner {
    consumer: Mutex<QueueConsumer>,
    backend: Mutex<Box<dyn BackendStream>>,
    node: InputNode,
    target_format: AudioFormat,
    next_seq: AtomicU64,
}

impl Drop for StreamInner {
    fn drop(&mut self) {
        let _ = self.backend.get_mut().close();
        unregister_stream(&self.node.id);
    }
}

/// Handle to one open capture stream.
#[derive(Clone)]
pub struct AudioStream {
    inner: Arc<StreamInner>,
}

impl AudioStream {
    pub(crate) fn new(
        consumer: QueueConsumer,
        backend: Box<dyn BackendStream>,
        node: InputNode,
        target_format: AudioFormat,
    ) -> Self {
        let inner = Arc::new(StreamInner {
            consumer: Mutex::new(consumer),
            backend: Mutex::new(backend),
            node,
            target_format,
            next_seq: AtomicU64::new(0),
        });
        #[cfg(not(any(test, feature = "test-util")))]
        register_stream(inner.node.id.clone(), Arc::downgrade(&inner));
        Self { inner }
    }

    #[cfg(not(any(test, feature = "test-util")))]
    pub(crate) fn from_inner(inner: Arc<StreamInner>) -> Self {
        Self { inner }
    }

    /// Non-blocking read: drain whatever is buffered (possibly empty).
    pub fn read(&mut self) -> Result<Vec<StreamItem>, Error> {
        let mut consumer = self.inner.consumer.lock();
        let mut items = Vec::new();
        while let Some(mut item) = consumer.pop() {
            if let StreamItem::Frame(frame) = &mut item {
                frame.seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
            }
            items.push(item);
        }
        Ok(items)
    }

    /// Blocking read with timeout; returns at least one item if available.
    pub fn read_timeout(&mut self, timeout: Duration) -> Result<Vec<StreamItem>, Error> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let items = self.read()?;
            if !items.is_empty() {
                return Ok(items);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The node this stream captures from.
    pub fn node(&self) -> &InputNode {
        &self.inner.node
    }

    /// The configured target format.
    pub fn target_format(&self) -> &AudioFormat {
        &self.inner.target_format
    }

    /// Close the stream, releasing resources and clearing buffers.
    pub fn close(self) -> Result<(), Error> {
        // Dropping self drops the Arc; if this is the last reference, StreamInner::Drop closes the backend.
        drop(self);
        Ok(())
    }
}

impl Drop for AudioStream {
    fn drop(&mut self) {
        // The actual close/teardown happens in StreamInner::Drop when the Arc count reaches zero.
    }
}
