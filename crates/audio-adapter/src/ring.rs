use crate::frame::{AudioFrame, StreamEvent, StreamItem};
use crate::format::{AudioFormat, SampleFormat};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

const FADE_DURATION: Duration = Duration::from_millis(5);

/// Bounded in-memory audio buffer with drop-oldest overrun policy.
pub struct AudioQueue {
    inner: Arc<Mutex<QueueState>>,
    format: AudioFormat,
}

struct QueueState {
    items: VecDeque<StreamItem>,
    capacity_bytes: usize,
    current_bytes: usize,
    dropped_bytes: usize,
}

impl AudioQueue {
    /// Create a new bounded queue with the given target format and byte capacity.
    pub fn new(format: AudioFormat, capacity_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueState {
                items: VecDeque::new(),
                capacity_bytes: capacity_bytes.max(1),
                current_bytes: 0,
                dropped_bytes: 0,
            })),
            format,
        }
    }

    /// Split into producer/consumer pair.
    pub fn split(self) -> (QueueProducer, QueueConsumer) {
        (
            QueueProducer {
                inner: self.inner.clone(),
                format: self.format.clone(),
            },
            QueueConsumer {
                inner: self.inner,
                format: self.format,
            },
        )
    }
}

/// Producer side.
pub struct QueueProducer {
    inner: Arc<Mutex<QueueState>>,
    format: AudioFormat,
}

impl QueueProducer {
    pub fn push(&mut self, item: StreamItem) {
        let byte_size = item_byte_size(&item);
        let mut state = self.inner.lock();

        // Drop oldest items until there is room.
        while state.current_bytes + byte_size > state.capacity_bytes && !state.items.is_empty() {
            if let Some(oldest) = state.items.pop_front() {
                state.current_bytes = state.current_bytes.saturating_sub(item_byte_size(&oldest));
                state.dropped_bytes += item_byte_size(&oldest);
            }
        }

        if state.current_bytes + byte_size <= state.capacity_bytes {
            state.current_bytes += byte_size;
            // Smooth boundary when dropping has occurred.
            if state.dropped_bytes > 0 {
                let mut item = item;
                smooth_boundary(&mut item);
                state.items.push_back(item);
            } else {
                state.items.push_back(item);
            }
        } else {
            // Item is larger than total capacity; drop it.
            state.dropped_bytes += byte_size;
        }
    }

    pub fn push_frame(
        &mut self,
        data: Vec<u8>,
        timestamp: Duration,
        duration: Duration,
        seq: u64,
    ) {
        self.push(StreamItem::Frame(AudioFrame {
            data,
            format: self.format.clone(),
            timestamp,
            duration,
            seq,
        }));
    }

    pub fn push_silence(&mut self, timestamp: Duration, duration: Duration, seq: u64) {
        let bytes = self.format.bytes_for_duration(duration);
        let mut data = vec![0u8; bytes];
        apply_fade(&mut data, &self.format);
        self.push(StreamItem::Frame(AudioFrame {
            data,
            format: self.format.clone(),
            timestamp,
            duration,
            seq,
        }));
    }

    pub fn push_event(&mut self, event: StreamEvent) {
        self.push(StreamItem::Event(event));
    }

    pub fn format(&self) -> &AudioFormat {
        &self.format
    }
}

/// Consumer side.
pub struct QueueConsumer {
    inner: Arc<Mutex<QueueState>>,
    format: AudioFormat,
}

impl QueueConsumer {
    pub fn pop(&mut self) -> Option<StreamItem> {
        let mut state = self.inner.lock();
        if state.dropped_bytes > 0 {
            let dropped = state.dropped_bytes;
            state.dropped_bytes = 0;
            return Some(StreamItem::Event(StreamEvent::Overrun {
                dropped: self.format.duration_for_bytes(dropped),
            }));
        }
        if let Some(item) = state.items.pop_front() {
            state.current_bytes = state.current_bytes.saturating_sub(item_byte_size(&item));
            Some(item)
        } else {
            None
        }
    }

    pub fn pop_timeout(&mut self, timeout: Duration) -> Option<StreamItem> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(item) = self.pop() {
                return Some(item);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn is_empty(&self) -> bool {
        let state = self.inner.lock();
        state.items.is_empty() && state.dropped_bytes == 0
    }

    pub fn available(&self) -> usize {
        self.inner.lock().items.len()
    }
}

fn item_byte_size(item: &StreamItem) -> usize {
    match item {
        StreamItem::Frame(f) => f.data.len(),
        StreamItem::Event(_) => 0,
    }
}

fn apply_fade(data: &mut [u8], format: &AudioFormat) {
    let fade_samples = (FADE_DURATION.as_secs_f64() * format.sample_rate as f64) as usize;
    match format.sample_format {
        SampleFormat::S16LE => {
            let samples: &mut [i16] = bytemuck::cast_slice_mut(data);
            let channels = format.channels as usize;
            let fade_frames = fade_samples / channels.max(1);
            let total_frames = samples.len() / channels.max(1);
            fade_s16(samples, channels, fade_frames, total_frames);
        }
        SampleFormat::F32LE => {
            let samples: &mut [f32] = bytemuck::cast_slice_mut(data);
            let channels = format.channels as usize;
            let fade_frames = fade_samples / channels.max(1);
            let total_frames = samples.len() / channels.max(1);
            fade_f32(samples, channels, fade_frames, total_frames);
        }
    }
}

fn fade_s16(samples: &mut [i16], channels: usize, fade_frames: usize, total_frames: usize) {
    let fade_frames = fade_frames.min(total_frames / 2);
    if fade_frames == 0 {
        return;
    }
    for f in 0..fade_frames {
        let gain = 0.5 * (1.0 - (std::f64::consts::PI * f as f64 / fade_frames as f64).cos());
        let g = gain as f32;
        for c in 0..channels {
            samples[f * channels + c] = (samples[f * channels + c] as f32 * g) as i16;
            let back = total_frames - 1 - f;
            samples[back * channels + c] = (samples[back * channels + c] as f32 * g) as i16;
        }
    }
}

fn fade_f32(samples: &mut [f32], channels: usize, fade_frames: usize, total_frames: usize) {
    let fade_frames = fade_frames.min(total_frames / 2);
    if fade_frames == 0 {
        return;
    }
    for f in 0..fade_frames {
        let gain = 0.5 * (1.0 - (std::f64::consts::PI * f as f64 / fade_frames as f64).cos());
        let g = gain as f32;
        for c in 0..channels {
            samples[f * channels + c] *= g;
            let back = total_frames - 1 - f;
            samples[back * channels + c] *= g;
        }
    }
}

fn smooth_boundary(item: &mut StreamItem) {
    if let StreamItem::Frame(frame) = item {
        apply_fade(&mut frame.data, &frame.format);
    }
}
