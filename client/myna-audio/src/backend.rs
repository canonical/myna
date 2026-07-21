//! The capture-backend seam (audio-adapter-api §5). A [`CaptureBackend`] opens
//! the device and delivers raw PCM through a [`Producer`]; the adapter core
//! ([`crate::CaptureSource`]) owns everything behind it — re-chunking, the
//! stats tap, the bounded ring. Backends: [`crate::ScriptedBackend`] (fake,
//! T50), `PipeWireBackend` (native, T52 — the sole live-capture backend).

use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use myna_core::{AudioFormat, CaptureError, PcmChunk, StopHandle};
use tokio::sync::watch;

use crate::ring::Ring;
use crate::stats::AudioStats;

/// What to capture. The adapter passes this through from its builder.
pub struct CaptureSpec {
    /// Produce EXACTLY this; the backend owns any conversion (§7).
    pub format: AudioFormat,
    /// PipeWire node to capture from, by stable `node.name`; `None` = default.
    pub target: Option<String>,
    /// Channel indices to pick/downmix on multi-channel devices (§9). Honored
    /// by the native backend (T52); the subprocess backend must error on
    /// `Some` rather than silently capture the wrong channels.
    pub channels: Option<Vec<u8>>,
    /// Graceful-stop flag; the backend must observe it within ~250 ms.
    pub stop: StopHandle,
}

/// Where a backend delivers PCM. `push` is synchronous and never blocks —
/// callable from a tokio task, a plain thread, or a realtime callback.
/// Overflow is the ring's problem (drop-oldest), never the backend's.
pub struct Producer {
    ring: Arc<Ring>,
    stats: watch::Sender<AudioStats>,
    format: AudioFormat,
    chunk_bytes: usize,
    frame_bytes: usize,
    pending: BytesMut,
    captured: Duration,
    session_peak: f32,
}

impl Producer {
    pub(crate) fn new(
        ring: Arc<Ring>,
        stats: watch::Sender<AudioStats>,
        format: AudioFormat,
        chunk_bytes: usize,
        frame_bytes: usize,
    ) -> Self {
        Self {
            ring,
            stats,
            format,
            chunk_bytes,
            frame_bytes,
            pending: BytesMut::new(),
            captured: Duration::ZERO,
            session_peak: 0.0,
        }
    }

    /// Deliver raw PCM (any buffer size; the adapter re-chunks to whole-frame
    /// ~100 ms chunks). Returns `false` once the consumer is gone or capture
    /// has ended — the backend should stop producing.
    pub fn push(&mut self, data: Bytes) -> bool {
        if self.ring.is_terminated() {
            return false;
        }
        self.pending.extend_from_slice(&data);
        while self.pending.len() >= self.chunk_bytes {
            let data = self.pending.split_to(self.chunk_bytes).freeze();
            self.emit(data);
        }
        !self.ring.is_terminated()
    }

    /// End capture: clean (`None`) after a graceful stop / device EOF, or
    /// fatal (`Some`) — becomes the stream's single `Err`. Pending whole
    /// frames flush as a final short chunk; a trailing partial frame (a
    /// misbehaving backend) is dropped, not padded.
    pub fn finish(mut self, fault: Option<CaptureError>) {
        let whole = self.pending.len() - self.pending.len() % self.frame_bytes;
        if whole > 0 {
            let data = self.pending.split_to(whole).freeze();
            self.emit(data);
        }
        self.ring.finish(fault);
    }

    fn emit(&mut self, data: Bytes) {
        let chunk = PcmChunk::new(data, self.format);
        let (rms, peak, clipped) = levels(&chunk);
        self.captured += chunk.duration();
        self.session_peak = self.session_peak.max(peak);
        // The buffer never drops (it grows to hold everything), so no audio
        // is ever lost; `dropped` stays zero. `push` returns the buffer
        // high-water mark, which we don't surface as a stat today.
        let _ = self.ring.push(chunk);
        let _ = self.format.bytes_per_second();
        let _ = self.stats.send(AudioStats {
            rms,
            peak,
            session_peak: self.session_peak,
            clipped,
            captured: self.captured,
            dropped: Duration::ZERO,
        });
    }
}

/// A capture backend: opens the device and produces raw PCM in exactly
/// `spec.format`, pushing into `producer` from wherever it runs.
pub trait CaptureBackend: Send {
    /// Must return quickly (spawn a task/thread for the capture loop). A
    /// failure to *open* is the `Err` here; a failure *during* capture goes
    /// through `producer.finish(Some(..))`.
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer) -> Result<(), CaptureError>;
}

/// Per-chunk levels, linear full-scale (§8). S16LE only — other widths report
/// silent levels until T33 settles the encoding story.
fn levels(chunk: &PcmChunk) -> (f32, f32, bool) {
    if chunk.format.sample_width_bytes != 2 || chunk.data.len() < 2 {
        return (0.0, 0.0, false);
    }
    let mut sum_sq = 0f64;
    let mut peak = 0i32;
    let mut clipped = false;
    for sample in chunk.data.chunks_exact(2) {
        let v = i16::from_le_bytes([sample[0], sample[1]]) as i32;
        let mag = v.abs();
        peak = peak.max(mag);
        clipped |= mag >= i16::MAX as i32;
        sum_sq += (v as f64) * (v as f64);
    }
    let n = (chunk.data.len() / 2) as f64;
    let rms = ((sum_sq / n).sqrt() / 32768.0) as f32;
    (rms, peak as f32 / 32768.0, clipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16_chunk(sample: i16, count: usize) -> PcmChunk {
        let mut data = Vec::with_capacity(count * 2);
        for _ in 0..count {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        PcmChunk::new(data, AudioFormat::default())
    }

    #[test]
    fn full_scale_square_is_loud_and_clipped() {
        let (rms, peak, clipped) = levels(&s16_chunk(i16::MAX, 1600));
        assert!(rms > 0.999 && peak > 0.999);
        assert!(clipped);
    }

    #[test]
    fn tenth_scale_signal_reads_a_tenth() {
        let (rms, peak, clipped) = levels(&s16_chunk(3277, 1600));
        assert!((rms - 0.1).abs() < 0.01, "rms {rms}");
        assert!((peak - 0.1).abs() < 0.01, "peak {peak}");
        assert!(!clipped);
    }

    #[test]
    fn silence_is_silent() {
        assert_eq!(levels(&s16_chunk(0, 1600)), (0.0, 0.0, false));
    }
}
