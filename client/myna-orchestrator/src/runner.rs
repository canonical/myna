//! One-utterance dictation runner (plan T41) — composes the three boundary
//! traits with the FSM driver: an [`AudioSource`] pushes PCM, the
//! [`run_session`] FSM driver mediates the backend, and a [`TextSink`] renders
//! the results. This is the reusable core of the demo binary and the seam T21
//! (hotkey → this) and T22 (this → injector) plug into.
//!
//! The audio-adapter contract maps cleanly: a clean source end (hotkey release
//! / WAV EOF) becomes `EndOfAudio` (finalize), and a capture fault becomes
//! `CaptureFailed` — a visible `Failed` outcome (abandon the backend session,
//! commit nothing, but tell the user *why*), never a silent abort.
//!
//! Capture starts at the press, before the backend is ready, and its buffer
//! holds the audio until the driver reads it. Capture health is watched
//! independently of that, so a fault or overload surfaces at once, not after
//! readiness and a drain.

use futures_util::StreamExt;
use myna_core::{CaptureHealth, SessionConfig};
use tokio::sync::mpsc;

use crate::audio::AudioSource;
use crate::backend::{BackendClient, BackendError};
use crate::driver::{run_session, OrchestratorControl, OrchestratorInput};
use crate::fsm::SessionOutcome;
use crate::sink::TextSink;
use crate::task::TaskGuard;

/// Capacity of the audio backlog between the source and the FSM before
/// backpressure applies (~1.6 s at 100 ms chunks); beyond it, audio waits in
/// the capture buffer.
const INPUT_CAPACITY: usize = 16;
/// Control carries at most a capture end and a capture fault per utterance.
const CONTROL_CAPACITY: usize = 4;
const OUTPUT_CAPACITY: usize = 64;

/// Run a single utterance to completion: open a backend session, stream every
/// chunk `source` produces, signal end-of-audio (or fail on a capture fault),
/// and forward every orchestrator event to `sink`. Returns the session outcome.
///
/// `config.audio_format` is overwritten with the source's actual format, so the
/// service validates against what is really being sent (the source never
/// resamples).
///
/// Dropping the returned future cancels capture and the backend transport.
pub async fn run_dictation<B, S, T>(
    backend: &B,
    mut config: SessionConfig,
    source: S,
    sink: &mut T,
) -> Result<SessionOutcome, BackendError>
where
    B: BackendClient,
    S: AudioSource + 'static,
    T: TextSink,
{
    config.audio_format = source.format();

    let (in_tx, in_rx) = mpsc::channel(INPUT_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel(CONTROL_CAPACITY);
    let (out_tx, mut out_rx) = mpsc::channel(OUTPUT_CAPACITY);

    let capture = TaskGuard::spawn(pump_capture(source, in_tx, control_tx));

    // Drive the FSM and drain its events to the sink concurrently.
    let driver = run_session(backend, config, in_rx, control_rx, out_tx);
    tokio::pin!(driver);

    let mut outputs_open = true;
    let outcome = loop {
        tokio::select! {
            event = out_rx.recv(), if outputs_open => match event {
                Some(event) => sink.emit(event).await,
                None => outputs_open = false,
            },
            result = &mut driver => {
                // Driver finished: drain any events still buffered, then return.
                while let Some(event) = out_rx.recv().await {
                    sink.emit(event).await;
                }
                break result;
            }
        }
    };

    // The session is terminal; whatever capture still holds is not wanted.
    capture.cancel().await;
    outcome
}

/// Capture from the press: move audio into `audio` as fast as the driver takes
/// it, and report the capture's end or fault on `control` as soon as health
/// shows it, without waiting for the audio queued ahead of it.
async fn pump_capture<S: AudioSource>(
    source: S,
    audio: mpsc::Sender<OrchestratorInput>,
    control: mpsc::Sender<OrchestratorControl>,
) {
    let mut health = source.health();
    let mut stream = Box::new(source).capture(); // the press: capture fills from here
    myna_core::dbg_log!("capture", "stream opened");
    let mut health_open = true;
    let mut ended = false;
    let mut pending: Option<OrchestratorInput> = None;
    let mut chunks = 0u64;
    let mut bytes = 0u64;
    loop {
        tokio::select! {
            biased;
            state = health.next(), if health_open => match state {
                Some(CaptureHealth::Faulted(fault)) => {
                    myna_core::dbg_log!("capture", "capture fault: {fault}");
                    let message = fault.to_string();
                    let _ = control.send(OrchestratorControl::CaptureFailed { message }).await;
                    return;
                }
                Some(CaptureHealth::Ended) if !ended => {
                    ended = true;
                    let _ = control.send(OrchestratorControl::CaptureEnded).await;
                }
                Some(_) => {}
                None => health_open = false,
            },
            permit = audio.reserve(), if pending.is_some() => {
                let (Ok(permit), Some(item)) = (permit, pending.take()) else {
                    return; // the driver is gone
                };
                let end = matches!(item, OrchestratorInput::EndOfAudio);
                permit.send(item);
                if end {
                    return;
                }
            }
            item = stream.next(), if pending.is_none() => match item {
                Some(Ok(chunk)) => {
                    chunks += 1;
                    bytes += chunk.data.len() as u64;
                    pending = Some(OrchestratorInput::Audio(chunk));
                }
                Some(Err(fault)) => {
                    myna_core::dbg_log!("capture", "capture fault: {fault}");
                    let message = fault.to_string();
                    let _ = control.send(OrchestratorControl::CaptureFailed { message }).await;
                    return;
                }
                None => {
                    myna_core::dbg_log!(
                        "capture",
                        "end of audio after {chunks} chunks / {bytes} bytes"
                    );
                    if !ended {
                        ended = true;
                        let _ = control.send(OrchestratorControl::CaptureEnded).await;
                    }
                    pending = Some(OrchestratorInput::EndOfAudio);
                }
            },
        }
    }
}

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WavFileSource;
    use crate::backend::fake::FakeBackend;
    use crate::fsm::OrchestratorEvent;
    use crate::sink::CollectingSink;
    use myna_core::AudioFormat;
    use std::path::PathBuf;

    fn wav_file(seconds_of_silence: usize) -> PathBuf {
        let fmt = AudioFormat::default();
        let data = vec![0u8; fmt.bytes_per_second() as usize * seconds_of_silence];
        let byte_rate = fmt.bytes_per_second();
        let block_align = (fmt.channels as u16) * (fmt.sample_width_bytes as u16);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(fmt.channels as u16).to_le_bytes());
        out.extend_from_slice(&fmt.sample_rate_hz.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&((fmt.sample_width_bytes as u16) * 8).to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);

        // Tests share this process; a clock reading is not a unique name.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("myna-runner-{}-{n}.wav", std::process::id()));
        std::fs::write(&path, out).unwrap();
        path
    }

    #[tokio::test]
    async fn wav_through_fake_backend_prints_transcript() {
        // The full T41 chain against a mock backend: WAV source → FSM → sink.
        let path = wav_file(1);
        let source = WavFileSource::new(&path).unwrap().with_chunk_seconds(0.1);
        let backend = FakeBackend::commit_drain();
        let mut sink = CollectingSink::default();

        let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "the quick brown fox jumps over the lazy dog.".into()
            }
        );
        assert_eq!(
            sink.finals(),
            vec!["the quick brown fox", "jumps over the lazy dog."]
        );
        assert_eq!(
            sink.done().as_deref(),
            Some("the quick brown fox jumps over the lazy dog.")
        );
        std::fs::remove_file(&path).ok();
    }

    /// T030 (feature 007): streaming round-trip — committed deltas arrive
    /// progressively at the sink; unstable deltas surface as Unstable, never
    /// as committed Final text; the terminal Done carries the full transcript.
    #[tokio::test]
    async fn streaming_committed_deltas_flow_to_sink_progressively() {
        use crate::backend::fake::FakeStep;
        use myna_core::{
            Disposition, Progress, TranscriptionEvent, TranscriptionFinal, PHASE_READY,
        };

        let committed = |text: &str, i: u32| {
            TranscriptionEvent::Final(TranscriptionFinal {
                text: text.into(),
                segments: vec![],
                disposition: Disposition::Committed,
                segment_index: Some(i),
            })
        };
        let unstable = |text: &str| {
            TranscriptionEvent::Final(TranscriptionFinal {
                text: text.into(),
                segments: vec![],
                disposition: Disposition::Unstable,
                segment_index: None,
            })
        };

        let backend = FakeBackend::new(vec![
            FakeStep::Emit(TranscriptionEvent::Progress(Progress::phase(PHASE_READY))),
            FakeStep::Emit(committed("Many ", 0)),
            FakeStep::Emit(unstable("little wrinkl")),
            FakeStep::Emit(unstable("little wrinkles")),
            FakeStep::Emit(committed("little wrinkles ", 1)),
            FakeStep::WaitForFinish,
            FakeStep::Emit(TranscriptionEvent::Done(TranscriptionFinal {
                text: "Many little wrinkles".into(),
                ..Default::default()
            })),
        ]);

        let path = wav_file(1);
        let source = WavFileSource::new(&path).unwrap().with_chunk_seconds(0.1);
        let mut sink = CollectingSink::default();

        let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "Many little wrinkles".into()
            }
        );
        // Committed segments reached the sink progressively, in order.
        assert_eq!(sink.finals(), vec!["Many ", "little wrinkles "]);
        // Unstable hypotheses surfaced as Unstable — not committed text.
        let unstables: Vec<_> = sink
            .events
            .iter()
            .filter_map(|e| match e {
                OrchestratorEvent::Unstable(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(unstables, vec!["little wrinkl", "little wrinkles"]);
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn myna_audio_capture_source_drops_in_behind_the_same_trait() {
        // T50 acceptance: the real adapter crate (over its fake backend — the
        // "mock audio adapter") replaces WavFileSource with no runner changes.
        use myna_audio::{CaptureSource, ScriptedBackend, Step};
        use std::time::Duration;

        let backend_script = ScriptedBackend::new(vec![Step::Silence(Duration::from_millis(300))]);
        let source = CaptureSource::builder(AudioFormat::default())
            .backend(Box::new(backend_script))
            .build();
        let backend = FakeBackend::commit_drain();
        let mut sink = CollectingSink::default();

        let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "the quick brown fox jumps over the lazy dog.".into()
            }
        );
    }

    #[tokio::test]
    async fn capture_format_is_negotiated_into_config() {
        // A non-default WAV format must flow into the session config.
        let path = wav_file(1);
        let source = WavFileSource::new(&path).unwrap();
        let fmt = source.format();
        let backend = FakeBackend::happy_path();
        let mut sink = CollectingSink::default();
        // Pass a deliberately empty config; the runner fills audio_format.
        let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
            .await
            .unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed { .. }));
        assert_eq!(fmt, AudioFormat::default());
        std::fs::remove_file(&path).ok();
    }
}
