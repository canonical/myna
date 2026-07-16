use crate::backend::BackendStream;
use crate::error::Error;
use crate::format::AudioFormat;
use crate::frame::StreamItem;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueConsumer;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

/// Global registry of currently open streams, keyed by node id (FR-003: one
/// open stream per node; opening an already-open node returns the existing
/// handle). Compiled unconditionally so the idempotency guarantee is identical
/// in production and under test.
static OPEN_STREAMS: LazyLock<Mutex<HashMap<NodeId, Weak<StreamInner>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Idempotent ensure-open: return the live stream for `node` if one exists,
/// otherwise create one via `create` *while holding the registry lock*, so two
/// concurrent opens of the same node cannot both create a stream.
pub(crate) fn open_or_existing(
    node: InputNode,
    target_format: AudioFormat,
    create: impl FnOnce() -> Result<(QueueConsumer, Box<dyn BackendStream>), Error>,
) -> Result<AudioStream, Error> {
    let mut registry = OPEN_STREAMS.lock();
    registry.retain(|_, weak| weak.strong_count() > 0);
    if let Some(existing) = registry.get(&node.id).and_then(Weak::upgrade) {
        return Ok(AudioStream { inner: existing });
    }
    let (consumer, backend) = create()?;
    let inner = Arc::new(StreamInner {
        consumer: Mutex::new(consumer),
        backend: Mutex::new(backend),
        node,
        target_format,
        closed: AtomicBool::new(false),
    });
    registry.insert(inner.node.id.clone(), Arc::downgrade(&inner));
    Ok(AudioStream { inner })
}

pub(crate) struct StreamInner {
    consumer: Mutex<QueueConsumer>,
    backend: Mutex<Box<dyn BackendStream>>,
    node: InputNode,
    target_format: AudioFormat,
    closed: AtomicBool,
}

impl StreamInner {
    /// Stop capture, release the source, clear buffers, and drop the registry
    /// entry — but only if the entry still refers to this stream, so a close
    /// racing a reopen never deletes the newer stream's registration.
    fn shutdown(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.backend.lock().close();
        self.consumer.lock().clear();
        let mut registry = OPEN_STREAMS.lock();
        if let Some(weak) = registry.get(&self.node.id) {
            if std::ptr::eq(weak.as_ptr(), self) {
                registry.remove(&self.node.id);
            }
        }
    }
}

impl Drop for StreamInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Handle to one open capture stream.
///
/// Handles obtained through the idempotent open share the same underlying
/// stream. `close()` on any handle stops capture for all of them (FR-008);
/// remaining handles read an empty, closed stream.
pub struct AudioStream {
    inner: Arc<StreamInner>,
}

impl AudioStream {
    /// Non-blocking read: drain whatever is buffered (possibly empty).
    /// Frames carry the timestamps and sequence numbers assigned at capture
    /// time; a sequence gap plus an `Overrun` event indicates dropped audio.
    pub fn read(&mut self) -> Result<Vec<StreamItem>, Error> {
        Ok(self.inner.consumer.lock().drain())
    }

    /// Blocking read: wait (on a condvar, not by polling) until at least one
    /// item is available or the timeout elapses, then drain.
    pub fn read_timeout(&mut self, timeout: Duration) -> Result<Vec<StreamItem>, Error> {
        let mut consumer = self.inner.consumer.lock();
        let mut items = Vec::new();
        if let Some(first) = consumer.pop_timeout(timeout) {
            items.push(first);
            items.extend(consumer.drain());
        }
        Ok(items)
    }

    /// The node this stream captures from.
    pub fn node(&self) -> &InputNode {
        &self.inner.node
    }

    /// The configured target format.
    pub fn target_format(&self) -> &AudioFormat {
        &self.inner.target_format
    }

    /// Whether the stream has been closed (by any handle or by device loss).
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Close the stream: stop capture, release the audio source, and clear
    /// buffers (FR-008, SC-004). Effective immediately, even if other handles
    /// to the same stream exist.
    pub fn close(self) -> Result<(), Error> {
        self.inner.shutdown();
        Ok(())
    }
}
