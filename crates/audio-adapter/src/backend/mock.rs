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

fn default_mock_node() -> InputNode {
    InputNode {
        id: NodeId::new("mock-default"),
        name: "mock-default".into(),
        description: "Mock default input".into(),
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

/// A deterministic backend for unit/contract tests.
pub struct MockBackend {
    nodes: Vec<InputNode>,
    /// When true, `open` will fail with `NoDevice`.
    pub fail_open: bool,
    /// When true, the stream will emit `DeviceLost` after `lose_after`.
    pub lose_after: Option<Duration>,
    /// Start time of an injected gap. Frames are not pushed between `gap_after`
    /// and `gap_after + gap_duration` to simulate a server underrun.
    pub gap_after: Option<Duration>,
    /// Duration of the injected gap. Requires `gap_after`.
    pub gap_duration: Option<Duration>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            nodes: vec![default_mock_node()],
            fail_open: false,
            lose_after: None,
            gap_after: None,
            gap_duration: None,
        }
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
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

impl AudioBackend for MockBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        Ok(self.nodes.clone())
    }

    fn open(&self, config: StreamConfig, mut producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error> {
        if self.fail_open {
            return Err(Error::NoDevice);
        }

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let format = config.target_format;
        let lose_after = self.lose_after;
        let gap_after = self.gap_after;
        let gap_duration = self.gap_duration;
        let start = Instant::now();

        let handle: JoinHandle<()> = thread::spawn(move || {
            let mut seq: u64 = 0;
            let mut timestamp = Duration::ZERO;
            let chunk_duration = Duration::from_millis(10);
            let chunk_bytes = format.bytes_for_duration(chunk_duration);
            let interval = chunk_duration;
            let mut next_time = Instant::now() + interval;

            while running_clone.load(Ordering::Relaxed) {
                if Instant::now() >= next_time {
                    if let Some(limit) = lose_after {
                        if start.elapsed() >= limit {
                            producer.push_event(StreamEvent::DeviceLost {
                                node: crate::node::NodeId::new("mock-default"),
                            });
                            break;
                        }
                    }

                    let elapsed = start.elapsed();
                    let (in_gap, entering_gap) = gap_after.map_or((false, false), |gap_start| {
                        let in_gap = gap_start <= elapsed
                            && gap_duration.is_some_and(|dur| elapsed < gap_start + dur);
                        let entering = in_gap
                            && (elapsed - gap_start < chunk_duration);
                        (in_gap, entering)
                    });

                    if entering_gap {
                        producer.push_event(StreamEvent::Underrun { filled: Duration::ZERO });
                    }

                    if in_gap {
                        producer.push_silence(timestamp, chunk_duration, seq);
                    } else {
                        let data = vec![0u8; chunk_bytes];
                        producer.push_frame(data, timestamp, chunk_duration, seq);
                    }
                    seq += 1;
                    timestamp += chunk_duration;
                    next_time += interval;
                }
                thread::sleep(Duration::from_micros(100));
            }
        });

        Ok(Box::new(MockStream { running, handle: Some(handle) }))
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
