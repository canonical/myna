//! The capture-backend seam. A [`CaptureBackend`] opens
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
use crate::voice::{VoiceTracker, FRAME as VOICE_FRAME};

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
/// Overload is the buffer's problem, never the backend's.
pub struct Producer {
    ring: Arc<Ring>,
    stats: watch::Sender<AudioStats>,
    format: AudioFormat,
    chunk_bytes: usize,
    frame_bytes: usize,
    pending: BytesMut,
    captured: Duration,
    session_peak: f32,
    voice: VoiceTracker,
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
            voice: VoiceTracker::default(),
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
        for (frame_rms, frame) in frame_levels(&chunk) {
            self.voice.observe(frame_rms, frame);
        }
        self.captured += chunk.duration();
        self.session_peak = self.session_peak.max(peak);
        self.ring.push(chunk);
        let _ = self.stats.send(AudioStats {
            rms,
            peak,
            session_peak: self.session_peak,
            clipped,
            captured: self.captured,
            noise_floor: self.voice.noise_floor(),
            speech_level: self.voice.speech_level(),
            last_voice: self.voice.last_voice(),
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

/// The chunk cut into [`VOICE_FRAME`]-long frames, each with its RMS and its
/// actual duration (the last frame of a short final chunk may be shorter).
/// Non-S16LE chunks yield nothing, like [`levels`].
fn frame_levels(chunk: &PcmChunk) -> Vec<(f32, Duration)> {
    if chunk.format.sample_width_bytes != 2 {
        return Vec::new();
    }
    let frame_bytes = (chunk.format.channels as usize * 2).max(1);
    let per_frame = (chunk.format.sample_rate_hz as f64 * VOICE_FRAME.as_secs_f64()) as usize;
    let bytes = (per_frame * frame_bytes).max(frame_bytes);
    chunk
        .data
        .chunks(bytes)
        .filter(|frame| frame.len() >= 2)
        .map(|frame| {
            let sum_sq: f64 = frame
                .chunks_exact(2)
                .map(|s| {
                    let v = i16::from_le_bytes([s[0], s[1]]) as f64;
                    v * v
                })
                .sum();
            let n = (frame.len() / 2) as f64;
            let rms = ((sum_sq / n).sqrt() / 32768.0) as f32;
            let duration = Duration::from_secs_f64(
                frame.len() as f64 / chunk.format.bytes_per_second().max(1) as f64,
            );
            (rms, duration)
        })
        .collect()
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

    #[test]
    fn a_chunk_cuts_into_twenty_millisecond_frames() {
        let frames = frame_levels(&s16_chunk(3277, 1600));
        assert_eq!(frames.len(), 5);
        for (rms, duration) in frames {
            assert!((rms - 0.1).abs() < 0.01, "rms {rms}");
            assert_eq!(duration, Duration::from_millis(20));
        }
    }

    #[test]
    fn a_short_final_chunk_keeps_its_partial_frame() {
        let frames = frame_levels(&s16_chunk(0, 400));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].1, Duration::from_millis(5));
    }

    #[test]
    fn wide_samples_yield_no_frames() {
        let chunk = PcmChunk::new(
            vec![0u8; 64],
            AudioFormat {
                sample_width_bytes: 4,
                ..AudioFormat::default()
            },
        );
        assert!(frame_levels(&chunk).is_empty());
    }
}
