//! The bounded in-memory ring between the capture backend and the consumer
//! stream (audio-adapter-api §6). It fills from hotkey press; the consumer
//! drains when it chooses (typically once the model is `ready`). Overflow is
//! **drop-oldest** — past the tolerated cold-load window the oldest audio is
//! the stalest, blocking would stall the capture path, and failing would turn
//! a slow model load into a user-facing error. Dropped audio is accounted so
//! the stats tap can surface it. Nothing here ever touches disk (invariant
//! §1.2); an abort discards the queue on the spot.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use myna_core::{CaptureError, PcmChunk};
use tokio::sync::Notify;

pub(crate) struct Ring {
    inner: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    queue: VecDeque<PcmChunk>,
    queued_bytes: usize,
    max_bytes: usize,
    /// Producer finished; no more pushes (clean end or fault).
    done: bool,
    /// A fatal fault, delivered once after the queued chunks drain.
    fault: Option<CaptureError>,
    /// Consumer gone (abort): everything discarded, producer told to stop.
    closed: bool,
    /// Running total of bytes aged out by drop-oldest.
    dropped_bytes: u64,
}

impl Ring {
    pub(crate) fn new(max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                queued_bytes: 0,
                max_bytes,
                done: false,
                fault: None,
                closed: false,
                dropped_bytes: 0,
            }),
            notify: Notify::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned mutex means a panic mid-push/pop; propagating the panic
        // is the honest failure mode for an in-process audio path.
        self.inner.lock().expect("audio ring mutex poisoned")
    }

    /// Producer side: enqueue a chunk, evicting the oldest past capacity.
    /// Returns the running dropped-bytes total for the stats tap. A push after
    /// the consumer is gone (or after `finish`) is a no-op.
    pub(crate) fn push(&self, chunk: PcmChunk) -> u64 {
        let dropped_bytes = {
            let mut inner = self.lock();
            if inner.closed || inner.done {
                return inner.dropped_bytes;
            }
            inner.queued_bytes += chunk.data.len();
            inner.queue.push_back(chunk);
            // Keep at least the newest chunk even if it alone exceeds the cap
            // (can't happen while max_bytes >= chunk_bytes, but stay safe).
            while inner.queued_bytes > inner.max_bytes && inner.queue.len() > 1 {
                if let Some(old) = inner.queue.pop_front() {
                    inner.queued_bytes -= old.data.len();
                    inner.dropped_bytes += old.data.len() as u64;
                }
            }
            inner.dropped_bytes
        };
        self.notify.notify_one();
        dropped_bytes
    }

    /// Producer side: capture is over — cleanly (`None`) or fatally (`Some`).
    pub(crate) fn finish(&self, fault: Option<CaptureError>) {
        {
            let mut inner = self.lock();
            if inner.done {
                return;
            }
            inner.done = true;
            if !inner.closed {
                inner.fault = fault;
            }
        }
        self.notify.notify_one();
    }

    /// True once the producer should stop pushing (consumer gone or finished).
    pub(crate) fn is_terminated(&self) -> bool {
        let inner = self.lock();
        inner.closed || inner.done
    }

    /// Consumer side: the next stream item per the §3 contract — queued chunks
    /// first, then a fault as one `Err`, then `None`.
    pub(crate) async fn next(&self) -> Option<Result<PcmChunk, CaptureError>> {
        loop {
            // Created before the check: a notify_one racing between our check
            // and the await stores a permit, so the wakeup is never lost.
            let notified = self.notify.notified();
            {
                let mut inner = self.lock();
                if let Some(chunk) = inner.queue.pop_front() {
                    inner.queued_bytes -= chunk.data.len();
                    return Some(Ok(chunk));
                }
                if let Some(fault) = inner.fault.take() {
                    return Some(Err(fault));
                }
                if inner.done || inner.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Consumer dropped (abort): discard everything, stop accepting pushes.
    pub(crate) fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.queue.clear();
        inner.queued_bytes = 0;
        inner.fault = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myna_core::AudioFormat;

    fn chunk(byte: u8, len: usize) -> PcmChunk {
        PcmChunk::new(vec![byte; len], AudioFormat::default())
    }

    #[test]
    fn evicts_oldest_past_capacity_and_accounts_drops() {
        let ring = Ring::new(200);
        for i in 0..5u8 {
            ring.push(chunk(i, 100));
        }
        // Capacity 200 at 100-byte chunks: only the two newest survive.
        let mut inner = ring.lock();
        assert_eq!(inner.queue.len(), 2);
        assert_eq!(inner.queue.pop_front().unwrap().data[0], 3);
        assert_eq!(inner.queue.pop_front().unwrap().data[0], 4);
        assert_eq!(inner.dropped_bytes, 300);
    }

    #[test]
    fn push_after_close_is_rejected() {
        let ring = Ring::new(200);
        ring.close();
        ring.push(chunk(1, 100));
        assert!(ring.is_terminated());
        assert_eq!(ring.lock().queue.len(), 0);
    }

    #[tokio::test]
    async fn drains_then_reports_fault_once() {
        let ring = Ring::new(1000);
        ring.push(chunk(1, 100));
        ring.finish(Some(CaptureError::Backend("boom".into())));
        assert!(matches!(ring.next().await, Some(Ok(_))));
        assert!(matches!(ring.next().await, Some(Err(CaptureError::Backend(_)))));
        assert!(ring.next().await.is_none());
    }
}
