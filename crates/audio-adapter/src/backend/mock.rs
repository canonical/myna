use crate::backend::{AudioBackend, BackendStream};
use crate::config::StreamConfig;
use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use crate::frame::StreamEvent;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueProducer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Amplitude of the constant test tone the mock produces (S16LE). A non-zero,
/// constant signal makes fades and silence fills observable in assertions.
pub const MOCK_TONE_AMPLITUDE: i16 = 1000;

fn mock_node(id: &str) -> InputNode {
    InputNode {
        id: NodeId::new(id),
        name: id.to_string(),
        description: format!("Mock input {id}"),
        is_default: true,
        supported_formats: vec![
            AudioFormat {
                sample_rate: 48_000,
                sample_format: SampleFormat::S16LE,
                channels: 2,
            },
            AudioFormat::default_target(),
        ],
    }
}

/// A deterministic backend for unit/contract tests. Always available; only
/// reachable through explicit injection (`open_stream_with_backend`).
pub struct MockBackend {
    nodes: Vec<InputNode>,
    /// When true, `open` fails with `NoDevice`.
    pub fail_open: bool,
    /// When set, the stream emits `DeviceLost` after this delay and stops.
    pub lose_after: Option<Duration>,
    /// Start of an injected server underrun: the mock stops pushing at this
    /// point and, when the gap ends, reports it through the shared
    /// `QueueProducer::note_gap` mechanism — exercising the same silence-fill
    /// path real backends use.
    pub gap_after: Option<Duration>,
    /// Duration of the injected gap. Requires `gap_after`.
    pub gap_duration: Option<Duration>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::with_node_id("mock-default")
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mock with a caller-chosen node id. Tests should use a unique id per
    /// test so the global idempotent-open registry never aliases streams
    /// across concurrently running tests.
    pub fn with_node_id(id: &str) -> Self {
        Self {
            nodes: vec![mock_node(id)],
            fail_open: false,
            lose_after: None,
            gap_after: None,
            gap_duration: None,
        }
    }

    pub fn with_nodes(nodes: Vec<InputNode>) -> Self {
        Self {
            nodes,
            fail_open: false,
            lose_after: None,
            gap_after: None,
            gap_duration: None,
        }
    }
}

fn tone_chunk(format: &AudioFormat, duration: Duration) -> Vec<u8> {
    let bytes = format.bytes_for_duration(duration);
    match format.sample_format {
        SampleFormat::S16LE => {
            let samples = vec![MOCK_TONE_AMPLITUDE; bytes / 2];
            bytemuck::cast_slice(&samples).to_vec()
        }
        SampleFormat::F32LE => {
            let value = MOCK_TONE_AMPLITUDE as f32 / i16::MAX as f32;
            let samples = vec![value; bytes / 4];
            bytemuck::cast_slice(&samples).to_vec()
        }
    }
}

impl AudioBackend for MockBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        Ok(self.nodes.clone())
    }

    fn open(
        &self,
        config: StreamConfig,
        producer: QueueProducer,
    ) -> Result<Box<dyn BackendStream>, Error> {
        if self.fail_open {
            return Err(Error::NoDevice);
        }

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let format = config.target_format;
        let node_id = self.nodes.first().map(|n| n.id.clone());
        let lose_after = self.lose_after;
        let gap_after = self.gap_after;
        let gap_duration = self.gap_duration.unwrap_or_default();
        let start = Instant::now();

        let handle: JoinHandle<()> = thread::spawn(move || {
            let chunk_duration = Duration::from_millis(10);
            let chunk = tone_chunk(&format, chunk_duration);
            let mut next_time = Instant::now() + chunk_duration;
            let mut gap_reported = false;

            while running_clone.load(Ordering::Relaxed) {
                if Instant::now() >= next_time {
                    let elapsed = start.elapsed();

                    if let Some(limit) = lose_after {
                        if elapsed >= limit {
                            producer.push_event(StreamEvent::DeviceLost {
                                node: node_id.clone().unwrap_or_else(|| NodeId::new("mock")),
                            });
                            break;
                        }
                    }

                    let in_gap = gap_after
                        .is_some_and(|g| elapsed >= g && elapsed < g + gap_duration);
                    if in_gap {
                        // Simulate a server underrun by simply not producing.
                    } else {
                        if gap_after.is_some_and(|g| elapsed >= g + gap_duration)
                            && !gap_reported
                        {
                            // Gap just ended: report it via the shared mechanism.
                            producer.note_gap(gap_duration);
                            gap_reported = true;
                        }
                        producer.push_frame(chunk.clone());
                    }
                    next_time += chunk_duration;
                }
                thread::sleep(Duration::from_micros(100));
            }
        });

        Ok(Box::new(MockStream {
            running,
            handle: Some(handle),
        }))
    }
}

struct MockStream {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BackendStream for MockStream {
    fn close(&mut self) -> Result<(), Error> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Drop for MockStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
