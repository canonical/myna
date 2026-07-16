use crate::format::AudioFormat;
use crate::node::NodeId;
use std::time::Duration;

/// A contiguous chunk of target-format audio with timing metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    /// Interleaved audio samples in the target format.
    pub data: Vec<u8>,
    /// Echo of the target format.
    pub format: AudioFormat,
    /// Start time of this frame relative to stream open.
    pub timestamp: Duration,
    /// Monotonically increasing sequence number assigned at capture time.
    /// A gap in delivered sequence numbers means frames were dropped (see
    /// `StreamEvent::Overrun`).
    pub seq: u64,
}

/// Out-of-band notifications interleaved with frames in the read results.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamEvent {
    /// Oldest buffered frames were dropped to make room.
    Overrun { dropped: Duration },
    /// Synthetic silence was inserted to cover a server underrun.
    Underrun { filled: Duration },
    /// The input node was lost; the stream is closed.
    DeviceLost { node: NodeId },
    /// Voice activity transition (only emitted when VAD is enabled).
    VoiceActivity { speaking: bool, at: Duration },
}

/// Item returned by reading from a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamItem {
    Frame(AudioFrame),
    Event(StreamEvent),
}

impl AudioFrame {
    /// Number of frames (samples per channel) represented by the data.
    pub fn frame_count(&self) -> usize {
        self.data.len() / self.format.frame_size_bytes()
    }

    /// Duration represented by `data`, derived from the payload and format so
    /// it can never disagree with the actual sample count.
    pub fn duration(&self) -> Duration {
        self.format.duration_for_bytes(self.data.len())
    }
}
