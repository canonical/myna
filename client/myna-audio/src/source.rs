//! [`CaptureSource`] — the adapter itself (audio-adapter-api §4): one public
//! type implementing `AudioSource` over any [`CaptureBackend`], composing the
//! re-chunker, the stats tap, and the bounded pre-ready ring.
//!
//! `capture()` is the hotkey press: the backend starts and the ring fills the
//! moment it is called. The consumer may defer polling (the accept-gate);
//! nothing is lost up to the ring depth. `stop()` on the handle drains then
//! ends; dropping the stream aborts and discards (§3/§6).

use std::sync::Arc;
use std::time::Duration;

use myna_core::{AudioFormat, AudioSource, CaptureStream, StopHandle};
use tokio::sync::watch;

use crate::backend::{CaptureBackend, CaptureSpec, Producer};
use crate::ring::Ring;
use crate::stats::AudioStats;

/// Default ring depth (§6): comfortably above the 5.8 s worst measured model
/// cold load, trivial memory (10 s at 16 kHz mono S16LE = 320 KB).
/// **Provisional** — the final default is one decision with T29.
pub const DEFAULT_RING_DEPTH: Duration = Duration::from_secs(10);

/// Default chunk duration (~100 ms, the prototype's value): small enough for
/// low latency, large enough to avoid per-chunk overhead.
pub const DEFAULT_CHUNK: Duration = Duration::from_millis(100);

/// The audio adapter: capture from a backend into a bounded ring, exactly one
/// format, with a stats tap. Construct via [`CaptureSource::builder`].
pub struct CaptureSource {
    format: AudioFormat,
    ring_depth: Duration,
    chunk: Duration,
    target: Option<String>,
    channels: Option<Vec<u8>>,
    backend: Box<dyn CaptureBackend>,
    stop: StopHandle,
    stats: watch::Sender<AudioStats>,
}

impl CaptureSource {
    /// Start building a source that produces exactly `format` — set by the
    /// dictation service from the STT service's advertised capabilities (§7);
    /// the adapter never chooses it.
    pub fn builder(format: AudioFormat) -> CaptureSourceBuilder {
        CaptureSourceBuilder {
            format,
            ring_depth: DEFAULT_RING_DEPTH,
            chunk: DEFAULT_CHUNK,
            target: None,
            channels: None,
            backend: None,
        }
    }

    /// The stats tap (§8): latest capture health, conflating by design (a
    /// level meter wants the latest value, not history).
    pub fn stats(&self) -> watch::Receiver<AudioStats> {
        self.stats.subscribe()
    }

    /// Graceful stop (hotkey release): drain everything captured, then end.
    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }
}

impl AudioSource for CaptureSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn capture(self: Box<Self>) -> CaptureStream {
        let frame_bytes =
            (self.format.channels as usize * self.format.sample_width_bytes as usize).max(1);
        let bps = self.format.bytes_per_second().max(1) as f64;
        let mut chunk_bytes = (bps * self.chunk.as_secs_f64()) as usize;
        chunk_bytes -= chunk_bytes % frame_bytes;
        chunk_bytes = chunk_bytes.max(frame_bytes);
        let max_bytes = ((bps * self.ring_depth.as_secs_f64()) as usize).max(chunk_bytes);

        let ring = Ring::new(max_bytes);
        let producer =
            Producer::new(ring.clone(), self.stats.clone(), self.format, chunk_bytes, frame_bytes);
        let spec = CaptureSpec {
            format: self.format,
            target: self.target,
            channels: self.channels,
            stop: self.stop.clone(),
        };

        // Failure to OPEN the device: the stream is its one Err, then None —
        // never an empty stream masquerading as a clean end (§3).
        if let Err(err) = self.backend.start(spec, producer) {
            return Box::pin(futures_util::stream::iter([Err(err)]));
        }

        // The guard rides inside the stream: dropping the stream (abort) trips
        // the stop flag for the backend and discards the ring.
        let guard = ConsumerGuard { ring, stop: self.stop };
        Box::pin(futures_util::stream::unfold(guard, |guard| async move {
            guard.ring.next().await.map(|item| (item, guard))
        }))
    }
}

struct ConsumerGuard {
    ring: Arc<Ring>,
    stop: StopHandle,
}

impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        self.stop.stop();
        self.ring.close();
    }
}

/// Builder for [`CaptureSource`]. A backend is required; everything else has
/// the documented defaults.
pub struct CaptureSourceBuilder {
    format: AudioFormat,
    ring_depth: Duration,
    chunk: Duration,
    target: Option<String>,
    channels: Option<Vec<u8>>,
    backend: Option<Box<dyn CaptureBackend>>,
}

impl CaptureSourceBuilder {
    /// Pre-ready ring depth (§6). Pair with the T29 residency default: the
    /// ring must cover the worst cold load the product tolerates.
    pub fn ring_depth(mut self, depth: Duration) -> Self {
        self.ring_depth = depth;
        self
    }

    /// Chunk duration (whole frames; default ~100 ms).
    pub fn chunk(mut self, chunk: Duration) -> Self {
        self.chunk = chunk;
        self
    }

    /// Capture from this PipeWire node (stable `node.name`, §9).
    pub fn target(mut self, node: impl Into<String>) -> Self {
        self.target = Some(node.into());
        self
    }

    /// Pick/downmix these channel indices on a multi-channel device (§9).
    pub fn channels(mut self, indices: Vec<u8>) -> Self {
        self.channels = Some(indices);
        self
    }

    /// The capture backend (§5) — required.
    pub fn backend(mut self, backend: Box<dyn CaptureBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// # Panics
    /// If no backend was set — a programming error, not a runtime condition.
    pub fn build(self) -> CaptureSource {
        let (stats, _) = watch::channel(AudioStats::default());
        CaptureSource {
            format: self.format,
            ring_depth: self.ring_depth,
            chunk: self.chunk,
            target: self.target,
            channels: self.channels,
            backend: self.backend.expect("CaptureSource requires a backend (§5)"),
            stop: StopHandle::default(),
            stats,
        }
    }
}
