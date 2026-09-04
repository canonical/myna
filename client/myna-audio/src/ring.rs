//! The in-memory capture buffer between the capture backend and the consumer
//! stream (audio-adapter-api §6). It fills from hotkey press; the consumer
//! drains when it chooses (typically once the model is `ready`).
//!
//! **It never silently drops captured audio.** The buffer exists to hold speech
//! across two lags that are both entirely normal:
//!
//! 1. the pre-ready cold-load window — the client captures from the moment the
//!    hotkey is pressed, but the server does not accept audio until its model
//!    is loaded (`ready`); and
//! 2. any transient in-session lag where the server briefly falls behind
//!    realtime.
//!
//! In both cases the buffer grows to hold everything and the consumer catches
//! up later. Dropping would silently lose the user's speech, which is never
//! acceptable for dictation. But the buffer is **bounded** so a persistently
//! slower-than-realtime service (a hardware tier that genuinely can't keep up)
//! can't grow it without limit: past `max_bytes` the buffer stops and the
//! stream ends with a [`CaptureError::Overloaded`] fault, so the client informs
//! the user rather than either losing speech or exhausting memory. The bound is
//! generous (see [`crate::source::DEFAULT_RING_DEPTH`]) so normal dictation,
//! including a slow cold load, never trips it. Nothing here ever touches disk
//! (invariant §1.2); an abort discards the queue on the spot.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use myna_core::{AudioFormat, CaptureError, PcmChunk};
use tokio::sync::Notify;

pub(crate) struct Ring {
    inner: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    queue: VecDeque<PcmChunk>,
    queued_bytes: usize,
    /// Hard bound on buffered audio. Past this the buffer stops growing and the
    /// stream faults with `Overloaded` (never a silent drop).
    max_bytes: usize,
    /// High-water mark of buffered audio, for the stats tap (diagnostics only).
    peak_bytes: usize,
    /// Format, to render the overloaded buffered-seconds in the fault.
    format: AudioFormat,
    /// Producer finished; no more pushes (clean end or fault).
    done: bool,
    /// A fatal fault, delivered once after the queued chunks drain.
    fault: Option<CaptureError>,
    /// Consumer gone (abort): everything discarded, producer told to stop.
    closed: bool,
}

impl Ring {
    pub(crate) fn new(max_bytes: usize, format: AudioFormat) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                queued_bytes: 0,
                max_bytes: max_bytes.max(1),
                peak_bytes: 0,
                format,
                done: false,
                fault: None,
                closed: false,
            }),
            notify: Notify::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned mutex means a panic mid-push/pop; propagating the panic
        // is the honest failure mode for an in-process audio path.
        self.inner.lock().expect("audio ring mutex poisoned")
    }

    /// Producer side: enqueue a chunk. Returns the current buffer high-water
    /// mark in bytes (for the stats tap). A push after the consumer is gone,
    /// after `finish`, or after overload is a no-op. **Never drops** — but past
    /// `max_bytes` it latches `overloaded` and stops accepting audio, so the
    /// stream ends with an `Overloaded` fault instead of growing without bound.
    pub(crate) fn push(&self, chunk: PcmChunk) -> u64 {
        let peak_bytes = {
            let mut inner = self.lock();
            if inner.closed || inner.done {
                return inner.peak_bytes as u64;
            }
            if inner.queued_bytes + chunk.data.len() > inner.max_bytes {
                // The service can't keep up: stop here and surface it. The
                // already-queued audio still drains before the fault, which is
                // delivered once via the normal `fault` path in `next`.
                inner.done = true;
                if !inner.closed {
                    let secs =
                        inner.peak_bytes as f64 / (inner.format.bytes_per_second().max(1) as f64);
                    inner.fault = Some(CaptureError::Overloaded(secs));
                }
                self.notify.notify_one();
                return inner.peak_bytes as u64;
            }
            inner.queued_bytes += chunk.data.len();
            inner.peak_bytes = inner.peak_bytes.max(inner.queued_bytes);
            inner.queue.push_back(chunk);
            inner.peak_bytes
        };
        self.notify.notify_one();
        peak_bytes as u64
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

    fn ring(max_bytes: usize) -> Arc<Ring> {
        Ring::new(max_bytes, AudioFormat::default())
    }

    #[tokio::test]
    async fn holds_everything_captured_before_the_consumer_drains() {
        // The load-bearing invariant: audio captured while the consumer is not
        // yet draining (the pre-ready cold-load window) is buffered in full,
        // never dropped — up to the (generous) overload bound.
        let r = ring(10_000_000);
        for i in 0..100u8 {
            r.push(chunk(i, 3200)); // 100 chunks, ~10 s at 16 kHz mono S16
        }
        r.finish(None);
        let mut got = Vec::new();
        while let Some(Ok(c)) = r.next().await {
            got.push(c.data[0]);
        }
        assert_eq!(
            got,
            (0..100).collect::<Vec<u8>>(),
            "every captured chunk survives"
        );
    }

    #[tokio::test]
    async fn never_drops_even_when_producer_races_far_ahead_of_consumer() {
        let r = ring(10_000_000);
        r.push(chunk(0, 100));
        assert_eq!(r.next().await.unwrap().unwrap().data[0], 0);
        for i in 1..=1000u32 {
            let _ = r.push(chunk((i % 250) as u8, 100));
        }
        r.finish(None);
        let mut count = 0usize;
        while let Some(Ok(_)) = r.next().await {
            count += 1;
        }
        assert_eq!(count, 1000, "every chunk pushed after the first survives");
    }

    #[tokio::test]
    async fn overload_drains_queued_then_faults_never_silently_truncates() {
        // Past the bound: the already-queued audio still drains, then the stream
        // faults with `Overloaded` — the client is told, not silently truncated.
        let r = ring(250); // 2 x 100-byte chunks fit; the 3rd overflows
        assert_eq!(r.push(chunk(1, 100)), 100);
        assert_eq!(r.push(chunk(2, 100)), 200);
        r.push(chunk(3, 100)); // overflow: latches the fault, does not enqueue
                               // The two accepted chunks drain first...
        assert_eq!(r.next().await.unwrap().unwrap().data[0], 1);
        assert_eq!(r.next().await.unwrap().unwrap().data[0], 2);
        // ...then the overload fault, exactly once, then end.
        assert!(matches!(
            r.next().await,
            Some(Err(CaptureError::Overloaded(_)))
        ));
        assert!(r.next().await.is_none());
    }

    #[test]
    fn push_after_close_is_rejected() {
        let r = ring(10_000);
        r.close();
        r.push(chunk(1, 100));
        assert!(r.is_terminated());
        assert_eq!(r.lock().queue.len(), 0);
    }

    #[tokio::test]
    async fn drains_then_reports_fault_once() {
        let r = ring(10_000);
        r.push(chunk(1, 100));
        r.finish(Some(CaptureError::Backend("boom".into())));
        assert!(matches!(r.next().await, Some(Ok(_))));
        assert!(matches!(
            r.next().await,
            Some(Err(CaptureError::Backend(_)))
        ));
        assert!(r.next().await.is_none());
    }
}
