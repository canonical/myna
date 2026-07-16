use crate::format::{AudioFormat, SampleFormat};
use crate::frame::{AudioFrame, StreamEvent, StreamItem};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FADE_DURATION: Duration = Duration::from_millis(5);

/// Bounded in-memory audio buffer with drop-oldest overrun policy.
///
/// The queue owns the stream timeline: sequence numbers and timestamps are
/// assigned here, at push time, so exactly one component is responsible for
/// FR-013 continuity. Frames dropped by the overrun policy therefore leave a
/// visible gap in both timestamps and sequence numbers, flagged by an
/// `Overrun` event (FR-014).
pub struct AudioQueue {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<QueueState>,
    available: Condvar,
    format: AudioFormat,
}

struct QueueState {
    items: VecDeque<StreamItem>,
    capacity_bytes: usize,
    current_bytes: usize,
    /// Bytes lost in the current (not yet reported) overrun span.
    dropped_bytes: usize,
    /// Head-fade the next pushed real frame (set after a silence fill).
    fade_in_next_push: bool,
    next_seq: u64,
    /// Running timestamp assigned to the next pushed frame.
    clock: Duration,
}

impl AudioQueue {
    /// Create a new bounded queue with the given target format and byte capacity.
    pub fn new(format: AudioFormat, capacity_bytes: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(QueueState {
                    items: VecDeque::new(),
                    capacity_bytes: capacity_bytes.max(1),
                    current_bytes: 0,
                    dropped_bytes: 0,
                    fade_in_next_push: false,
                    next_seq: 0,
                    clock: Duration::ZERO,
                }),
                available: Condvar::new(),
                format,
            }),
        }
    }

    /// Split into producer/consumer pair.
    pub fn split(self) -> (QueueProducer, QueueConsumer) {
        (
            QueueProducer {
                shared: self.shared.clone(),
            },
            QueueConsumer {
                shared: self.shared,
            },
        )
    }
}

/// Producer side. Cloneable so a backend can push frames from its capture
/// callback and events (e.g. `DeviceLost`) from other callbacks.
#[derive(Clone)]
pub struct QueueProducer {
    shared: Arc<Shared>,
}

impl QueueProducer {
    /// Push one chunk of target-format interleaved audio. The queue assigns
    /// the frame's timestamp and sequence number.
    pub fn push_frame(&self, data: Vec<u8>) {
        let format = self.shared.format.clone();
        let duration = format.duration_for_bytes(data.len());
        let mut state = self.shared.state.lock();
        let mut frame = AudioFrame {
            data,
            format,
            timestamp: state.clock,
            seq: state.next_seq,
        };
        state.next_seq += 1;
        state.clock += duration;
        if state.fade_in_next_push {
            fade_in(&mut frame.data, &self.shared.format);
            state.fade_in_next_push = false;
        }
        insert(&mut state, StreamItem::Frame(frame));
        drop(state);
        self.shared.available.notify_one();
    }

    /// Record a capture gap (server underrun): the missing span is filled with
    /// silence so the delivered timeline stays continuous, an
    /// `Underrun { filled }` event is queued, and the boundaries between real
    /// audio and the silent span are smoothed (FR-018/FR-015).
    pub fn note_gap(&self, gap: Duration) {
        if gap.is_zero() {
            return;
        }
        let format = self.shared.format.clone();
        let bytes = format.bytes_for_duration(gap);
        let silence_duration = format.duration_for_bytes(bytes);
        let mut state = self.shared.state.lock();
        // Fade out the tail of the most recent real frame still queued, if any.
        if let Some(StreamItem::Frame(f)) = state
            .items
            .iter_mut()
            .rev()
            .find(|i| matches!(i, StreamItem::Frame(_)))
        {
            fade_out(&mut f.data, &format);
        }
        insert(
            &mut state,
            StreamItem::Event(StreamEvent::Underrun { filled: gap }),
        );
        let silence = AudioFrame {
            data: vec![0u8; bytes],
            format,
            timestamp: state.clock,
            seq: state.next_seq,
        };
        state.next_seq += 1;
        state.clock += silence_duration;
        insert(&mut state, StreamItem::Frame(silence));
        state.fade_in_next_push = true;
        drop(state);
        self.shared.available.notify_one();
    }

    /// Push an out-of-band event.
    pub fn push_event(&self, event: StreamEvent) {
        let mut state = self.shared.state.lock();
        insert(&mut state, StreamItem::Event(event));
        drop(state);
        self.shared.available.notify_one();
    }

    pub fn format(&self) -> &AudioFormat {
        &self.shared.format
    }
}

/// Insert an item, evicting the oldest *frames* (never events) when over
/// capacity. Events such as `DeviceLost` must survive overruns.
fn insert(state: &mut QueueState, item: StreamItem) {
    let size = item_byte_size(&item);
    if size > state.capacity_bytes {
        // Larger than the whole buffer: count it as dropped.
        state.dropped_bytes += size;
        return;
    }
    while state.current_bytes + size > state.capacity_bytes {
        let Some(idx) = state
            .items
            .iter()
            .position(|i| matches!(i, StreamItem::Frame(_)))
        else {
            break;
        };
        if let Some(StreamItem::Frame(f)) = state.items.remove(idx) {
            state.current_bytes = state.current_bytes.saturating_sub(f.data.len());
            state.dropped_bytes += f.data.len();
        }
    }
    state.current_bytes += size;
    state.items.push_back(item);
}

/// Consumer side.
pub struct QueueConsumer {
    shared: Arc<Shared>,
}

impl QueueConsumer {
    /// Pop the next item, if any. When an overrun span is pending, an
    /// `Overrun { dropped }` event is delivered first and the head of the
    /// first surviving frame is fade-in smoothed once — the splice repair for
    /// the discontinuity the drop created (FR-014/FR-015).
    pub fn pop(&mut self) -> Option<StreamItem> {
        let mut state = self.shared.state.lock();
        pop_locked(&mut state, &self.shared.format)
    }

    /// Pop with a bounded wait, blocking on a condvar (no polling).
    pub fn pop_timeout(&mut self, timeout: Duration) -> Option<StreamItem> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        loop {
            if let Some(item) = pop_locked(&mut state, &self.shared.format) {
                return Some(item);
            }
            if self
                .shared
                .available
                .wait_until(&mut state, deadline)
                .timed_out()
            {
                return pop_locked(&mut state, &self.shared.format);
            }
        }
    }

    /// Drain everything currently buffered.
    pub fn drain(&mut self) -> Vec<StreamItem> {
        let mut state = self.shared.state.lock();
        let mut items = Vec::new();
        while let Some(item) = pop_locked(&mut state, &self.shared.format) {
            items.push(item);
        }
        items
    }

    /// Discard all buffered items (used on stream close, FR-008).
    pub fn clear(&mut self) {
        let mut state = self.shared.state.lock();
        state.items.clear();
        state.current_bytes = 0;
        state.dropped_bytes = 0;
    }

    pub fn is_empty(&self) -> bool {
        let state = self.shared.state.lock();
        state.items.is_empty() && state.dropped_bytes == 0
    }
}

fn pop_locked(state: &mut QueueState, format: &AudioFormat) -> Option<StreamItem> {
    if state.dropped_bytes > 0 {
        let dropped = state.dropped_bytes;
        state.dropped_bytes = 0;
        // Splice repair: smooth the head of the first surviving frame.
        if let Some(StreamItem::Frame(f)) = state
            .items
            .iter_mut()
            .find(|i| matches!(i, StreamItem::Frame(_)))
        {
            fade_in(&mut f.data, format);
        }
        return Some(StreamItem::Event(StreamEvent::Overrun {
            dropped: format.duration_for_bytes(dropped),
        }));
    }
    let item = state.items.pop_front()?;
    state.current_bytes = state
        .current_bytes
        .saturating_sub(item_byte_size(&item));
    Some(item)
}

fn item_byte_size(item: &StreamItem) -> usize {
    match item {
        StreamItem::Frame(f) => f.data.len(),
        StreamItem::Event(_) => 0,
    }
}

fn fade_frames(format: &AudioFormat) -> usize {
    (FADE_DURATION.as_secs_f64() * format.sample_rate as f64) as usize
}

/// Raised-cosine fade-in over the head of `data`.
fn fade_in(data: &mut [u8], format: &AudioFormat) {
    apply_ramp(data, format, true);
}

/// Raised-cosine fade-out over the tail of `data`.
fn fade_out(data: &mut [u8], format: &AudioFormat) {
    apply_ramp(data, format, false);
}

fn apply_ramp(data: &mut [u8], format: &AudioFormat, fade_in: bool) {
    let channels = (format.channels as usize).max(1);
    match format.sample_format {
        SampleFormat::S16LE => {
            let samples: &mut [i16] = bytemuck::cast_slice_mut(data);
            ramp(samples, channels, fade_frames(format), fade_in, |s, g| {
                (*s as f32 * g) as i16
            });
        }
        SampleFormat::F32LE => {
            let samples: &mut [f32] = bytemuck::cast_slice_mut(data);
            ramp(samples, channels, fade_frames(format), fade_in, |s, g| *s * g);
        }
    }
}

fn ramp<T: Copy>(
    samples: &mut [T],
    channels: usize,
    fade_frames: usize,
    fade_in: bool,
    scale: impl Fn(&T, f32) -> T,
) {
    let total_frames = samples.len() / channels;
    let n = fade_frames.min(total_frames);
    if n == 0 {
        return;
    }
    for i in 0..n {
        // gain ramps 0 -> 1 over the faded region.
        let gain =
            0.5 * (1.0 - (std::f64::consts::PI * i as f64 / n as f64).cos());
        let g = gain as f32;
        let frame = if fade_in { i } else { total_frames - 1 - i };
        for c in 0..channels {
            let idx = frame * channels + c;
            samples[idx] = scale(&samples[idx], g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> AudioFormat {
        AudioFormat::default_target()
    }

    fn tone(frames: usize, format: &AudioFormat) -> Vec<u8> {
        let samples = vec![1000i16; frames * format.channels as usize];
        bytemuck::cast_slice(&samples).to_vec()
    }

    #[test]
    fn timeline_is_assigned_by_the_queue() {
        let (producer, mut consumer) = AudioQueue::new(fmt(), 1 << 20).split();
        producer.push_frame(tone(160, &fmt()));
        producer.push_frame(tone(160, &fmt()));
        let items = consumer.drain();
        let frames: Vec<_> = items
            .into_iter()
            .filter_map(|i| match i {
                StreamItem::Frame(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].seq, 0);
        assert_eq!(frames[1].seq, 1);
        assert_eq!(frames[0].timestamp, Duration::ZERO);
        assert_eq!(frames[1].timestamp, frames[0].duration());
    }

    #[test]
    fn overrun_drops_oldest_emits_event_and_smooths_head() {
        let format = fmt();
        // Capacity for exactly two 10 ms frames.
        let capacity = format.bytes_for_duration(Duration::from_millis(20));
        let (producer, mut consumer) = AudioQueue::new(format.clone(), capacity).split();
        for _ in 0..4 {
            producer.push_frame(tone(160, &format));
        }
        let items = consumer.drain();
        // First item must be the overrun report.
        let StreamItem::Event(StreamEvent::Overrun { dropped }) = &items[0] else {
            panic!("expected Overrun first, got {:?}", items[0]);
        };
        assert_eq!(*dropped, Duration::from_millis(20));
        // Surviving frames keep their original (gapped) seq/timestamps.
        let frames: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                StreamItem::Frame(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].seq, 2, "oldest frames dropped, not newest");
        // Splice head is faded in: first sample near zero, mid-frame intact.
        let samples: &[i16] = bytemuck::cast_slice(&frames[0].data);
        assert!(samples[0].abs() < 200, "head not faded: {}", samples[0]);
        assert_eq!(samples[100], 1000, "fade must not touch mid-frame audio");
        // The later frame is untouched.
        let samples: &[i16] = bytemuck::cast_slice(&frames[1].data);
        assert_eq!(samples[0], 1000);
    }

    #[test]
    fn events_survive_overrun_eviction() {
        let format = fmt();
        let capacity = format.bytes_for_duration(Duration::from_millis(10));
        let (producer, mut consumer) = AudioQueue::new(format.clone(), capacity).split();
        producer.push_frame(tone(160, &format));
        producer.push_event(StreamEvent::DeviceLost {
            node: crate::node::NodeId::new("n"),
        });
        // Overflow the queue repeatedly; the event must survive.
        for _ in 0..3 {
            producer.push_frame(tone(160, &format));
        }
        let items = consumer.drain();
        assert!(
            items
                .iter()
                .any(|i| matches!(i, StreamItem::Event(StreamEvent::DeviceLost { .. }))),
            "DeviceLost event was evicted by the overrun policy"
        );
    }

    #[test]
    fn note_gap_fills_silence_keeps_timeline_and_smooths_boundaries() {
        let format = fmt();
        let (producer, mut consumer) = AudioQueue::new(format.clone(), 1 << 20).split();
        producer.push_frame(tone(160, &format));
        producer.note_gap(Duration::from_millis(20));
        producer.push_frame(tone(160, &format));
        let items = consumer.drain();

        // Expect: faded-tail frame, Underrun event, silence frame, faded-head frame.
        let mut frames = Vec::new();
        let mut underrun = None;
        for item in items {
            match item {
                StreamItem::Frame(f) => frames.push(f),
                StreamItem::Event(StreamEvent::Underrun { filled }) => underrun = Some(filled),
                _ => {}
            }
        }
        assert_eq!(underrun, Some(Duration::from_millis(20)));
        assert_eq!(frames.len(), 3);
        // Timeline continuous through the silence.
        assert_eq!(frames[1].timestamp, frames[0].timestamp + frames[0].duration());
        assert_eq!(frames[2].timestamp, frames[1].timestamp + frames[1].duration());
        assert_eq!(frames[0].seq + 1, frames[1].seq);
        assert_eq!(frames[1].seq + 1, frames[2].seq);
        // Tail of the pre-gap frame faded out.
        let pre: &[i16] = bytemuck::cast_slice(&frames[0].data);
        assert!(pre[pre.len() - 1].abs() < 200, "tail not faded: {}", pre[pre.len() - 1]);
        // Silence frame is silent.
        let silence: &[i16] = bytemuck::cast_slice(&frames[1].data);
        assert!(silence.iter().all(|s| *s == 0));
        // Head of the post-gap frame faded in.
        let post: &[i16] = bytemuck::cast_slice(&frames[2].data);
        assert!(post[0].abs() < 200, "head not faded: {}", post[0]);
        assert_eq!(post[100], 1000);
    }

    #[test]
    fn pop_timeout_blocks_until_push() {
        let format = fmt();
        let (producer, mut consumer) = AudioQueue::new(format.clone(), 1 << 20).split();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            producer.push_frame(vec![0u8; 320]);
        });
        let item = consumer.pop_timeout(Duration::from_millis(500));
        assert!(item.is_some());
        t.join().unwrap();
    }
}
