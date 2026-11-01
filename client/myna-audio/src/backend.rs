//! The capture-backend seam. A [`CaptureBackend`] opens
//! the device and delivers raw PCM through a [`Producer`]; the adapter core
//! ([`crate::CaptureSource`]) owns everything behind it — re-chunking, the
//! stats tap, the bounded ring. Backends: [`crate::ScriptedBackend`] (fake,
//! T50), `PipeWireBackend` (native, T52 — the sole live-capture backend).

use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use myna_core::{AudioFormat, CaptureError, CaptureHealth, PcmChunk, StopHandle};
use tokio::sync::watch;

use crate::ring::Ring;
use crate::snr::SnrEstimator;
use crate::stats::AudioStats;
use crate::voice::{HighPass, VoiceTracker, FRAME as VOICE_FRAME, HIGH_PASS_HZ};

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

/// Where a backend delivers PCM. `push` is synchronous and bounded but not
/// realtime-safe: it copies, allocates and takes the ring's mutex, so call it
/// from a tokio task or a plain thread, never a realtime callback. Overload
/// is the buffer's problem, never the backend's.
///
/// The producer also publishes [`CaptureHealth`]: `Capturing` on the first
/// non-empty push, `Faulted` on overload or a fatal finish, `Ended` on a clean
/// one. Dropping it without [`Producer::finish`] is a backend fault, so a lost
/// producer never leaves the consumer waiting forever.
pub struct Producer {
    ring: Arc<Ring>,
    stats: watch::Sender<AudioStats>,
    health: watch::Sender<CaptureHealth>,
    concluded: bool,
    format: AudioFormat,
    chunk_bytes: usize,
    frame_bytes: usize,
    pending: BytesMut,
    captured: Duration,
    session_peak: f32,
    voice: VoiceTracker,
    voice_filter: HighPass,
    snr: SnrEstimator,
    offset: OffsetMedian,
}

impl Producer {
    pub(crate) fn new(
        ring: Arc<Ring>,
        stats: watch::Sender<AudioStats>,
        health: watch::Sender<CaptureHealth>,
        format: AudioFormat,
        chunk_bytes: usize,
        frame_bytes: usize,
    ) -> Self {
        Self {
            ring,
            stats,
            health,
            concluded: false,
            format,
            chunk_bytes,
            frame_bytes,
            pending: BytesMut::new(),
            captured: Duration::ZERO,
            session_peak: 0.0,
            voice: VoiceTracker::default(),
            voice_filter: HighPass::new(
                format.sample_rate_hz,
                format.channels.into(),
                HIGH_PASS_HZ,
            ),
            snr: SnrEstimator::new(format.sample_rate_hz, format.channels.into()),
            offset: OffsetMedian::default(),
        }
    }

    /// Deliver raw PCM (any buffer size; the adapter re-chunks to whole-frame
    /// ~100 ms chunks). Returns `false` once the consumer is gone or capture
    /// has ended — the backend should stop producing.
    pub fn push(&mut self, data: Bytes) -> bool {
        if self.ring.is_terminated() {
            return false;
        }
        if !data.is_empty() {
            self.publish(CaptureHealth::Capturing);
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
        self.conclude(fault);
    }

    fn conclude(&mut self, fault: Option<CaptureError>) {
        if std::mem::replace(&mut self.concluded, true) {
            return;
        }
        let whole = self.pending.len() - self.pending.len() % self.frame_bytes;
        if whole > 0 {
            let data = self.pending.split_to(whole).freeze();
            self.emit(data);
        }
        let terminal = match &fault {
            Some(err) if !self.ring.is_closed() => CaptureHealth::Faulted(err.clone()),
            _ => CaptureHealth::Ended,
        };
        self.publish(terminal);
        self.ring.finish(fault);
    }

    /// Advance health; terminal states are never left.
    fn publish(&self, next: CaptureHealth) {
        self.health.send_if_modified(|current| {
            let terminal = matches!(current, CaptureHealth::Faulted(_) | CaptureHealth::Ended);
            if terminal || *current == next {
                return false;
            }
            *current = next;
            true
        });
    }

    fn emit(&mut self, data: Bytes) {
        let chunk = PcmChunk::new(data, self.format);
        let (rms, peak, clipped) = levels(&chunk);
        let (sum, count) = sample_sum(&chunk);
        self.offset.observe(sum, count);
        for (frame_rms, frame) in frame_levels(&chunk, &mut self.voice_filter) {
            self.voice.observe(frame_rms, frame);
        }
        self.snr.observe(&chunk);
        self.captured += chunk.duration();
        self.session_peak = self.session_peak.max(peak);
        if let Some(overload) = self.ring.push(chunk) {
            self.publish(CaptureHealth::Faulted(overload));
        }
        let _ = self.stats.send(AudioStats {
            rms,
            peak,
            session_peak: self.session_peak,
            clipped,
            captured: self.captured,
            noise_floor: self.voice.noise_floor(),
            speech_level: self.voice.speech_level(),
            offset: self.offset.median(),
            snr_db: self.snr.snr_db(),
            last_voice: self.voice.last_voice(),
        });
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.conclude(Some(CaptureError::Backend(
            "capture backend exited without reporting an outcome".into(),
        )));
    }
}

/// A capture backend: opens the device and produces raw PCM in exactly
/// `spec.format`, pushing into `producer` from wherever it runs.
pub trait CaptureBackend: Send {
    /// Must return promptly without waiting for the device (spawn a task or
    /// thread for opening and capture). Every outcome, including a failure to
    /// open, goes through `producer.finish`.
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer);
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

/// The chunk's samples summed (linear full scale) and counted; S16LE only,
/// like [`levels`].
const OFFSET_BINS: usize = 2001;

/// The session's DC offset: the median of its chunks' mean samples, weighted
/// by sample count. A lasting offset (issue #64) holds most chunks; a thump as
/// the stream opens holds a few, and over a short dictation a plain mean read
/// one as -20 dBFS of offset. Bins 0.001 of full scale wide keep a long
/// session's memory fixed, and each bin sums its means so a steady offset
/// reads exactly.
struct OffsetMedian {
    weights: Vec<u64>,
    sums: Vec<f64>,
    total: u64,
}

impl Default for OffsetMedian {
    fn default() -> Self {
        Self {
            weights: vec![0; OFFSET_BINS],
            sums: vec![0.0; OFFSET_BINS],
            total: 0,
        }
    }
}

impl OffsetMedian {
    fn observe(&mut self, sum: f64, count: u64) {
        if count == 0 {
            return;
        }
        let mean = sum / count as f64;
        // An S16 mean lies in [-1, 32767/32768]: bins 0 to 2000.
        let bin = ((mean + 1.0) * 1000.0).round() as usize;
        self.weights[bin] += count;
        self.sums[bin] += sum;
        self.total += count;
    }

    fn median(&self) -> f32 {
        let half = self.total.div_ceil(2);
        let mut seen = 0;
        for (&weight, &sum) in self.weights.iter().zip(&self.sums) {
            seen += weight;
            if weight > 0 && seen >= half {
                return (sum / weight as f64) as f32;
            }
        }
        0.0
    }
}

fn sample_sum(chunk: &PcmChunk) -> (f64, u64) {
    if chunk.format.sample_width_bytes != 2 {
        return (0.0, 0);
    }
    let samples = chunk.data.chunks_exact(2);
    let count = samples.len() as u64;
    let sum = samples
        .map(|s| i16::from_le_bytes([s[0], s[1]]) as f64 / 32768.0)
        .sum();
    (sum, count)
}

/// The chunk cut into [`VOICE_FRAME`]-long frames, each with its RMS above
/// [`crate::voice::HIGH_PASS_HZ`] (through `filter`, which carries over from
/// chunk to chunk) and its actual duration (the last frame of a short final chunk may be shorter).
/// Non-S16LE chunks yield nothing, like [`levels`].
fn frame_levels(chunk: &PcmChunk, filter: &mut HighPass) -> Vec<(f32, Duration)> {
    if chunk.format.sample_width_bytes != 2 {
        return Vec::new();
    }
    let channels = (chunk.format.channels as usize).max(1);
    let frame_bytes = channels * 2;
    let per_frame = (chunk.format.sample_rate_hz as f64 * VOICE_FRAME.as_secs_f64()) as usize;
    let bytes = (per_frame * frame_bytes).max(frame_bytes);
    chunk
        .data
        .chunks(bytes)
        .filter(|frame| frame.len() >= 2)
        .map(|frame| {
            let sum_sq: f64 = frame
                .chunks_exact(2)
                .enumerate()
                .map(|(i, s)| {
                    let v = i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0;
                    let v = filter.filter(i % channels, v) as f64;
                    v * v
                })
                .sum();
            let n = (frame.len() / 2) as f64;
            let rms = (sum_sq / n).sqrt() as f32;
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

    /// `seconds` of mono 16 kHz audio, sample `n` of which is `f(n / 16 kHz)`.
    fn s16_signal(seconds: f32, mut f: impl FnMut(f32) -> f32) -> PcmChunk {
        let count = (seconds * 16_000.0) as usize;
        let mut data = Vec::with_capacity(count * 2);
        for n in 0..count {
            let v = (f(n as f32 / 16_000.0) * 32768.0).round() as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
        PcmChunk::new(data, AudioFormat::default())
    }

    fn tone(hz: f32, amplitude: f32) -> impl Fn(f32) -> f32 {
        move |t| amplitude * (2.0 * std::f32::consts::PI * hz * t).sin()
    }

    fn voice_filter() -> HighPass {
        HighPass::new(16_000, 1, HIGH_PASS_HZ)
    }

    #[test]
    fn a_chunk_cuts_into_twenty_millisecond_frames() {
        // A 1 kHz tone of 0.1 amplitude is 0.0707 RMS; speech frequencies
        // pass the filter.
        let frames = frame_levels(&s16_signal(0.1, tone(1_000.0, 0.1)), &mut voice_filter());
        assert_eq!(frames.len(), 5);
        for (rms, duration) in frames {
            assert!((rms - 0.0707).abs() < 0.003, "rms {rms}");
            assert_eq!(duration, Duration::from_millis(20));
        }
    }

    #[test]
    fn a_short_final_chunk_keeps_its_partial_frame() {
        let frames = frame_levels(&s16_chunk(0, 400), &mut voice_filter());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].1, Duration::from_millis(5));
    }

    #[test]
    fn a_dc_offset_is_not_a_level() {
        // Two chunks, so the filter state crosses a chunk boundary unbroken.
        let mut filter = voice_filter();
        for _ in 0..2 {
            for (rms, _) in frame_levels(&s16_chunk(-7168, 1600), &mut filter) {
                assert!(rms < 1e-4, "rms {rms}");
            }
        }
    }

    #[test]
    fn rumble_below_the_cutoff_is_attenuated() {
        let frames = frame_levels(&s16_signal(1.0, tone(15.0, 0.1)), &mut voice_filter());
        let loudest = frames.iter().map(|(rms, _)| *rms).fold(0.0, f32::max);
        assert!(loudest < 0.02, "loudest {loudest}");
    }

    #[test]
    fn an_offset_microphone_in_a_quiet_room_is_not_voice() {
        // Issue #64's microphone: a -13 dBFS offset that drifts, under
        // -60 dBFS of room tone. Unfiltered it reads as a word from 200 ms.
        let mut noise = 1u32;
        let room = move |t: f32| {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            -0.219
                + 0.008 * (2.0 * std::f32::consts::PI * 15.0 * t).sin()
                + 0.001 * (noise as f32 / u32::MAX as f32 - 0.5)
        };
        let mut voice = VoiceTracker::default();
        for (rms, frame) in frame_levels(&s16_signal(60.0, room), &mut voice_filter()) {
            voice.observe(rms, frame);
        }
        assert_eq!(voice.last_voice(), None);
    }

    #[test]
    fn speech_over_an_offset_is_still_voice() {
        let speech = |t: f32| {
            -0.219
                + if t > 0.5 {
                    0.05 * (2.0 * std::f32::consts::PI * 300.0 * t).sin()
                } else {
                    0.0
                }
        };
        let mut voice = VoiceTracker::default();
        for (rms, frame) in frame_levels(&s16_signal(1.5, speech), &mut voice_filter()) {
            voice.observe(rms, frame);
        }
        assert_eq!(voice.last_voice(), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn stereo_channels_are_filtered_apart() {
        // Left sits on an offset, right carries a tone: interleaved through
        // one shared state the offset would leak in as a 8 kHz square wave.
        let mut data = Vec::new();
        for n in 0..1600 {
            let right = (tone(1_000.0, 0.1)(n as f32 / 16_000.0) * 32768.0) as i16;
            data.extend_from_slice(&(-7168i16).to_le_bytes());
            data.extend_from_slice(&right.to_le_bytes());
        }
        let format = AudioFormat {
            channels: 2,
            ..AudioFormat::default()
        };
        let frames = frame_levels(
            &PcmChunk::new(data, format),
            &mut HighPass::new(16_000, 2, HIGH_PASS_HZ),
        );
        for (rms, _) in frames {
            // Both channels' samples in one RMS: the tone's 0.0707 over two.
            assert!((rms - 0.05).abs() < 0.003, "rms {rms}");
        }
    }

    #[test]
    fn the_sample_sum_is_signed_and_counts_every_sample() {
        let (sum, count) = sample_sum(&s16_chunk(-7168, 1600));
        assert_eq!(count, 1600);
        assert!((sum / count as f64 + 0.21875).abs() < 1e-9, "{sum}");
        assert!(sample_sum(&s16_signal(0.1, tone(1_000.0, 0.1))).0.abs() < 0.01);
    }

    #[test]
    fn no_samples_is_no_offset() {
        let mut offset = OffsetMedian::default();
        assert_eq!(offset.median(), 0.0);
        offset.observe(0.0, 0);
        assert_eq!(offset.median(), 0.0);
    }

    #[test]
    fn the_offset_is_the_median_chunk_weighted_by_samples() {
        let mut offset = OffsetMedian::default();
        offset.observe(-0.5 * 100.0, 100);
        offset.observe(0.25 * 300.0, 300);
        offset.observe(0.0, 150);
        assert!((offset.median() - 0.25).abs() < 1e-6, "{}", offset.median());
    }

    #[test]
    fn full_scale_means_land_in_the_end_bins() {
        let mut offset = OffsetMedian::default();
        offset.observe(-1.0, 1);
        assert_eq!(offset.median(), -1.0);
        let mut offset = OffsetMedian::default();
        offset.observe(32767.0 / 32768.0, 1);
        assert!((offset.median() - 32767.0 / 32768.0).abs() < 1e-6);
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
        assert!(frame_levels(&chunk, &mut voice_filter()).is_empty());
        assert_eq!(sample_sum(&chunk), (0.0, 0));
    }
}
