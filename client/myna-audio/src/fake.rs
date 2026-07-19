//! [`ScriptedBackend`] — the fake capture backend (T50), a **permanent
//! fixture** in the same spirit as the Python `FakeAdapter` and the
//! orchestrator's `FakeBackend`: deterministic, model- and hardware-free, it
//! pins the `CaptureBackend` contract so adapter behavior (ring, stats,
//! lifecycle) is testable with no PipeWire anywhere. It is also the mock
//! audio adapter for orchestrator work: `CaptureSource` over a script stands
//! in for a live microphone.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use myna_core::{AudioFormat, CaptureError};

use crate::backend::{CaptureBackend, CaptureSpec, Producer};

/// One scripted capture action.
pub enum Step {
    /// Push zeroed PCM of this duration in the negotiated format.
    Silence(Duration),
    /// Push raw bytes as-is (whole frames, or the trailing remainder is
    /// dropped at finish per the §4 re-chunk rule).
    Bytes(Vec<u8>),
    /// Pace like a live device. Interruptible: the stop flag is honored
    /// within ~50 ms, well inside the ~250 ms promptness contract (§5).
    Wait(Duration),
    /// Fail capture fatally with `CaptureError::Backend(msg)`.
    Fault(String),
}

/// A scripted [`CaptureBackend`].
pub struct ScriptedBackend {
    steps: Vec<Step>,
    unavailable: Option<String>,
    finished: Arc<AtomicBool>,
}

impl ScriptedBackend {
    pub fn new(steps: Vec<Step>) -> Self {
        Self { steps, unavailable: None, finished: Arc::new(AtomicBool::new(false)) }
    }

    /// A backend whose device cannot be opened: `start()` fails with
    /// `DeviceUnavailable` and the capture stream is one `Err`, then `None`.
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self { steps: Vec::new(), unavailable: Some(msg.into()), finished: Arc::new(AtomicBool::new(false)) }
    }

    /// Test probe: set once the capture task has exited — how tests observe
    /// that a stop/abort actually reached the backend.
    pub fn finished(&self) -> Arc<AtomicBool> {
        self.finished.clone()
    }
}

impl CaptureBackend for ScriptedBackend {
    fn start(self: Box<Self>, spec: CaptureSpec, mut producer: Producer) -> Result<(), CaptureError> {
        if let Some(msg) = self.unavailable {
            return Err(CaptureError::DeviceUnavailable(msg));
        }
        let finished = self.finished;
        let steps = self.steps;
        tokio::spawn(async move {
            let mut fault = None;
            'script: for step in steps {
                if spec.stop.is_stopped() {
                    break;
                }
                match step {
                    Step::Silence(duration) => {
                        if !producer.push(silence(&spec.format, duration)) {
                            break;
                        }
                    }
                    Step::Bytes(bytes) => {
                        if !producer.push(Bytes::from(bytes)) {
                            break;
                        }
                    }
                    Step::Wait(duration) => {
                        let mut remaining = duration;
                        while !remaining.is_zero() {
                            let slice = remaining.min(Duration::from_millis(50));
                            tokio::time::sleep(slice).await;
                            remaining -= slice;
                            if spec.stop.is_stopped() {
                                break 'script;
                            }
                        }
                    }
                    Step::Fault(msg) => {
                        fault = Some(CaptureError::Backend(msg));
                        break;
                    }
                }
            }
            producer.finish(fault);
            finished.store(true, Ordering::Release);
        });
        Ok(())
    }
}

/// Whole-frame zeroed PCM of `duration` in `format`.
fn silence(format: &AudioFormat, duration: Duration) -> Bytes {
    let frame = (format.channels as usize * format.sample_width_bytes as usize).max(1);
    let mut n = (format.bytes_per_second().max(1) as f64 * duration.as_secs_f64()) as usize;
    n -= n % frame;
    Bytes::from(vec![0u8; n])
}
