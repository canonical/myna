//! `FakeBackend` — the T40 in-process backend fixture, and a **permanent
//! regression asset** (the Rust mirror of the Python `FakeAdapter`).
//!
//! It implements [`BackendClient`] without a model, a socket, or audio
//! inspection: `open_session` spawns a task that plays a scripted sequence of
//! transcript events into the downstream channel while draining (and ignoring)
//! the audio the FSM pushes up. That lets the orchestrator FSM and its driver be
//! exercised end-to-end — including the full `STATUS` liveness sequence
//! (`loading → ready → transcribing`) and the async edge cases from
//! `docs/architecture/ie115-lifecycle.md` — with zero I/O and deterministic
//! output.
//!
//! Unlike the ws wire (where the pump treats any `transcription.error` as
//! terminal and closes), the fixture emits exactly the script it is given and
//! nothing more, so advisory/recoverable-error and commit-drain scenarios can be
//! staged precisely.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::Notify;

use myna_core::{
    ErrorData, Progress, SessionConfig, TranscriptionEvent, TranscriptionFinal, PHASE_PREPARING,
    PHASE_READY, PHASE_TRANSCRIBING, PROTOCOL_VERSION,
};

use super::{BackendClient, BackendError, BackendEvents, BackendHandle, BackendSink, Outbound};

const EVENT_CAPACITY: usize = 64;
const OUTBOUND_CAPACITY: usize = 16;

/// A step in a fake session script.
#[derive(Clone, Debug)]
pub enum FakeStep {
    /// Emit an event downstream immediately.
    Emit(TranscriptionEvent),
    /// Block until the FSM sends `session.finish` (or aborts). Models a backend
    /// that only decodes the tail after end-of-audio — the §3C commit-drain
    /// subtlety.
    WaitForFinish,
}

/// A scripted, model-free [`BackendClient`].
pub struct FakeBackend {
    script: Vec<FakeStep>,
}

impl FakeBackend {
    /// Build from an explicit script.
    pub fn new(script: Vec<FakeStep>) -> Self {
        Self { script }
    }

    /// The canonical happy path: the full `STATUS` liveness sequence, a UI
    /// snippet, two committed segments, and a terminal `done` — mirroring the
    /// Python fake adapter's `default_script` plus loading→ready liveness.
    pub fn happy_path() -> Self {
        Self::new(vec![
            FakeStep::Emit(loading()),
            FakeStep::Emit(ready()),
            FakeStep::Emit(transcribing()),
            FakeStep::Emit(snippet("The quick")),
            FakeStep::Emit(final_seg("The quick brown fox")),
            FakeStep::Emit(snippet("jumps over")),
            FakeStep::Emit(final_seg("jumps over the lazy dog.")),
            FakeStep::Emit(done("The quick brown fox jumps over the lazy dog.")),
        ])
    }

    /// §3C commit-drain: the model is ready and emits an early segment, but the
    /// tail final and `done` only come *after* `session.finish`.
    pub fn commit_drain() -> Self {
        Self::new(vec![
            FakeStep::Emit(loading()),
            FakeStep::Emit(ready()),
            FakeStep::Emit(final_seg("the quick brown fox")),
            FakeStep::WaitForFinish,
            FakeStep::Emit(final_seg("jumps over the lazy dog.")),
            FakeStep::Emit(done("the quick brown fox jumps over the lazy dog.")),
        ])
    }

    /// §3B error mid-stream: some progress, then a terminal error instead of a
    /// `done`.
    pub fn mid_stream_error(code: &str, message: &str) -> Self {
        Self::new(vec![
            FakeStep::Emit(loading()),
            FakeStep::Emit(ready()),
            FakeStep::Emit(final_seg("the quick")),
            FakeStep::Emit(error(code, message)),
        ])
    }

    /// §3A slow load: a run of `loading` before `ready`, so audio pushed early is
    /// gated out. (Determinism of the *drop* is covered by the pure FSM tests;
    /// this staging simply exercises the same sequence over the wire.)
    pub fn slow_ready() -> Self {
        Self::new(vec![
            FakeStep::Emit(loading()),
            FakeStep::WaitForFinish,
            FakeStep::Emit(ready()),
            FakeStep::Emit(final_seg("late")),
            FakeStep::Emit(done("late")),
        ])
    }
}

#[async_trait::async_trait]
impl BackendClient for FakeBackend {
    async fn open_session(&self, _config: SessionConfig) -> Result<BackendHandle, BackendError> {
        let (out_tx, out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let (ev_tx, ev_rx) =
            mpsc::channel::<Result<TranscriptionEvent, BackendError>>(EVENT_CAPACITY);
        tokio::spawn(pump(self.script.clone(), out_rx, ev_tx));
        Ok(BackendHandle {
            sink: BackendSink { tx: out_tx },
            events: BackendEvents { rx: ev_rx },
            protocol_version: Some(PROTOCOL_VERSION.to_string()),
        })
    }
}

/// Drives one fake session: a background drain reads outbound audio/control
/// (signalling on `session.finish`/abort), while the foreground plays the
/// script into the event channel.
async fn pump(
    script: Vec<FakeStep>,
    mut out_rx: mpsc::Receiver<Outbound>,
    ev_tx: mpsc::Sender<Result<TranscriptionEvent, BackendError>>,
) {
    let finished = Arc::new(Notify::new());
    let aborted = Arc::new(AtomicBool::new(false));

    let drain_finished = finished.clone();
    let drain_aborted = aborted.clone();
    let drain = tokio::spawn(async move {
        while let Some(outbound) = out_rx.recv().await {
            match outbound {
                Outbound::Audio(_) => {} // fixture ignores audio content
                Outbound::Finish => {
                    // `notify_one` stores a permit, so a `notified()` that runs
                    // *later* still wakes — no lost-wakeup race with WaitForFinish.
                    drain_finished.notify_one();
                }
                Outbound::Abort => {
                    drain_aborted.store(true, Ordering::SeqCst);
                    drain_finished.notify_one();
                    break;
                }
            }
        }
        // Sink dropped (FSM done) — release any pending WaitForFinish.
        drain_finished.notify_one();
    });

    for step in script {
        if aborted.load(Ordering::SeqCst) {
            break;
        }
        match step {
            FakeStep::Emit(event) => {
                if ev_tx.send(Ok(event)).await.is_err() {
                    break; // FSM dropped the receiver (terminal reached / abort)
                }
            }
            FakeStep::WaitForFinish => {
                finished.notified().await;
                if aborted.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    }

    // Dropping `ev_tx` closes the event stream; if the script didn't end in a
    // terminal event, the driver observes the close and fails the session.
    drop(ev_tx);
    let _ = drain.await;
}

// --- event constructors (kept here so the scripts read declaratively) --------

fn loading() -> TranscriptionEvent {
    TranscriptionEvent::Progress(Progress::phase(PHASE_PREPARING))
}

fn ready() -> TranscriptionEvent {
    TranscriptionEvent::Progress(Progress::phase(PHASE_READY))
}

fn transcribing() -> TranscriptionEvent {
    TranscriptionEvent::Progress(Progress::phase(PHASE_TRANSCRIBING))
}

fn snippet(text: &str) -> TranscriptionEvent {
    TranscriptionEvent::Progress(Progress {
        snippet: Some(text.into()),
        phase: PHASE_TRANSCRIBING.into(),
    })
}

fn final_seg(text: &str) -> TranscriptionEvent {
    TranscriptionEvent::Final(TranscriptionFinal {
        text: text.into(),
        segments: vec![],
        ..Default::default()
    })
}

fn done(text: &str) -> TranscriptionEvent {
    TranscriptionEvent::Done(TranscriptionFinal {
        text: text.into(),
        segments: vec![],
        ..Default::default()
    })
}

fn error(code: &str, message: &str) -> TranscriptionEvent {
    TranscriptionEvent::Error(ErrorData {
        code: code.into(),
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WavFileSource;
    use crate::fsm::SessionOutcome;
    use crate::runner::run_dictation;
    use crate::sink::CollectingSink;
    use myna_core::{AudioFormat, SessionConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Minimal in-memory canonical PCM WAV of `seconds` of silence.
    fn silence_wav(seconds: f64) -> std::path::PathBuf {
        let fmt = AudioFormat::default();
        let data = vec![0u8; (fmt.bytes_per_second() as f64 * seconds) as usize];
        let block_align = (fmt.channels as u16) * (fmt.sample_width_bytes as u16);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(fmt.channels as u16).to_le_bytes());
        out.extend_from_slice(&fmt.sample_rate_hz.to_le_bytes());
        out.extend_from_slice(&fmt.bytes_per_second().to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&((fmt.sample_width_bytes as u16) * 8).to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("myna-gate-{}-{}.wav", std::process::id(), nanos));
        std::fs::write(&path, out).unwrap();
        path
    }

    /// A backend that records whether any audio arrived *before* it emitted
    /// `ready`, then finishes cleanly. It delays `ready` behind a short sleep,
    /// so a client that streamed eagerly would push audio into the closed gate
    /// during the window.
    struct GateProbe {
        audio_before_ready: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl BackendClient for GateProbe {
        async fn open_session(
            &self,
            _config: SessionConfig,
        ) -> Result<BackendHandle, BackendError> {
            let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
            let (ev_tx, ev_rx) = mpsc::channel(EVENT_CAPACITY);
            let flag = self.audio_before_ready.clone();
            tokio::spawn(async move {
                let ready_emitted = Arc::new(AtomicBool::new(false));
                let drain_ready = ready_emitted.clone();
                let drain = tokio::spawn(async move {
                    while let Some(o) = out_rx.recv().await {
                        match o {
                            Outbound::Audio(_) => {
                                if !drain_ready.load(Ordering::SeqCst) {
                                    flag.store(true, Ordering::SeqCst);
                                }
                            }
                            Outbound::Finish | Outbound::Abort => break,
                        }
                    }
                });
                let _ = ev_tx.send(Ok(loading())).await;
                // A window in which an ungated client would leak audio early.
                tokio::time::sleep(Duration::from_millis(50)).await;
                ready_emitted.store(true, Ordering::SeqCst);
                let _ = ev_tx.send(Ok(ready())).await;
                let _ = drain.await; // audio streamed + finished
                let _ = ev_tx.send(Ok(final_seg("ok"))).await;
                let _ = ev_tx.send(Ok(done("ok"))).await;
            });
            Ok(BackendHandle {
                sink: BackendSink { tx: out_tx },
                events: BackendEvents { rx: ev_rx },
                protocol_version: Some(PROTOCOL_VERSION.to_string()),
            })
        }
    }

    /// Regression for the deadlock/mass-drop this fixture family exists to guard:
    /// the runner's client-side accept-gate must hold every chunk until the
    /// backend signals `ready`. Without the gate (audio streamed eagerly), the
    /// probe would see audio during its pre-ready window and the assert trips.
    #[tokio::test]
    async fn runner_gates_all_audio_until_ready() {
        let path = silence_wav(0.5);
        let source = WavFileSource::new(&path).unwrap().with_chunk_seconds(0.05);
        let audio_before_ready = Arc::new(AtomicBool::new(false));
        let backend = GateProbe { audio_before_ready: audio_before_ready.clone() };
        let mut sink = CollectingSink::default();

        let outcome =
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await.unwrap();

        assert!(
            !audio_before_ready.load(Ordering::SeqCst),
            "audio reached the backend before `ready` — the accept-gate leaked"
        );
        assert_eq!(outcome, SessionOutcome::Completed { transcript: "ok".into() });
        std::fs::remove_file(&path).ok();
    }

    /// A backend that delays `ready` (model still loading) and counts every
    /// audio byte the FSM forwards after the gate opens.
    struct CountingLateReady {
        received: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl BackendClient for CountingLateReady {
        async fn open_session(
            &self,
            _config: SessionConfig,
        ) -> Result<BackendHandle, BackendError> {
            use std::sync::atomic::Ordering;
            let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
            let (ev_tx, ev_rx) = mpsc::channel(EVENT_CAPACITY);
            let received = self.received.clone();
            tokio::spawn(async move {
                let _ = ev_tx.send(Ok(loading())).await;
                // The model "loads" for a beat while the client keeps capturing.
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = ev_tx.send(Ok(ready())).await;
                while let Some(o) = out_rx.recv().await {
                    match o {
                        Outbound::Audio(chunk) => {
                            received.fetch_add(chunk.data.len(), Ordering::SeqCst);
                        }
                        Outbound::Finish | Outbound::Abort => break,
                    }
                }
                let _ = ev_tx.send(Ok(final_seg("ok"))).await;
                let _ = ev_tx.send(Ok(done("ok"))).await;
            });
            Ok(BackendHandle {
                sink: BackendSink { tx: out_tx },
                events: BackendEvents { rx: ev_rx },
                protocol_version: Some(PROTOCOL_VERSION.to_string()),
            })
        }
    }

    /// Reproduction of the desktop "only the last few seconds land" bug: a live
    /// mic fills the capture ring while the server model loads (`ready` lags).
    /// Every captured byte must survive to reach the backend once the gate
    /// opens — the ring is a hold-during-load buffer, not a lossy rolling
    /// window (§6, and the explicit product requirement).
    #[tokio::test]
    async fn no_audio_is_dropped_when_ready_lags_capture() {
        use myna_audio::{CaptureSource, ScriptedBackend, Step};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fmt = AudioFormat::default();
        // ~1 s of audio (10 × 0.1 s), paced like a live mic. The default buffer
        // bound is generous, so nothing is dropped while `ready` lags.
        let bytes_per_chunk = (fmt.bytes_per_second() as usize) / 10; // 0.1 s
        let total_bytes = bytes_per_chunk * 10;
        let mut steps = Vec::new();
        for _ in 0..10 {
            steps.push(Step::Bytes(vec![7u8; bytes_per_chunk]));
            steps.push(Step::Wait(Duration::from_millis(20)));
        }
        let source = CaptureSource::builder(fmt)
            .backend(Box::new(ScriptedBackend::new(steps)))
            .build();

        let received = Arc::new(AtomicUsize::new(0));
        let backend = CountingLateReady { received: received.clone() };
        let mut sink = CollectingSink::default();

        let outcome =
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await.unwrap();

        assert!(matches!(outcome, SessionOutcome::Completed { .. }));
        assert_eq!(
            received.load(Ordering::SeqCst),
            total_bytes,
            "every captured byte must reach the backend — the ring must not drop \
             audio captured while the model was loading"
        );
    }
}
