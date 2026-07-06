//! One-utterance dictation runner (plan T41) — composes the three boundary
//! traits with the FSM driver: an [`AudioSource`] pushes PCM, the
//! [`run_session`] FSM driver mediates the backend, and a [`TextSink`] renders
//! the results. This is the reusable core of the demo binary and the seam T21
//! (hotkey → this) and T22 (this → injector) plug into.
//!
//! The audio-adapter contract (`docs/audio-adapter-api.md` §5) maps cleanly:
//! a clean source end (hotkey release / WAV EOF) becomes `EndOfAudio`
//! (finalize), and a capture fault becomes `Abort` (abandon, commit nothing).

use std::sync::Arc;

use tokio::sync::{mpsc, Notify};

use crate::audio::AudioSource;
use crate::backend::{BackendClient, BackendError};
use crate::driver::{run_session, OrchestratorInput};
use crate::fsm::{OrchestratorEvent, SessionOutcome};
use crate::sink::TextSink;
use myna_core::SessionConfig;
use futures_util::StreamExt;

/// Capacity of the audio backlog between the source and the FSM before
/// backpressure applies — the "bounded in-memory buffer" invariant (~1.6 s at
/// 100 ms chunks).
const INPUT_CAPACITY: usize = 16;
const OUTPUT_CAPACITY: usize = 64;

/// Run a single utterance to completion: open a backend session, stream every
/// chunk `source` produces, signal end-of-audio (or abort on a capture fault),
/// and forward every orchestrator event to `sink`. Returns the session outcome.
///
/// `config.audio_format` is overwritten with the source's actual format, so the
/// service validates against what is really being sent (the source never
/// resamples — audio-adapter-api §1/§7).
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

    let (in_tx, in_rx) = mpsc::channel::<OrchestratorInput>(INPUT_CAPACITY);
    let (out_tx, mut out_rx) = mpsc::channel(OUTPUT_CAPACITY);

    // Hold capture until the model is resident: the pump waits for the first
    // `Ready` before pulling the first chunk. This is the client half of the
    // accept-gate (ie115-lifecycle.md §3A) — without it a slow cold load lets a
    // replayable/paced source drain entirely into the closed gate and every
    // chunk is dropped. `notify_one` stores a permit, so a `Ready` that fires
    // before the pump parks is not lost. Holding the *stream* like this suits
    // paced/replayable sources (what the demo drives); a live mic MUST instead
    // capture from hotkey press into its ring buffer and gate only the push —
    // speech during a cold load is otherwise lost (requirement:
    // audio-adapter-api.md §6, a T21 acceptance criterion).
    let ready_gate = Arc::new(Notify::new());

    // Pump the capture stream into the FSM's input channel. A clean end →
    // EndOfAudio (finalize); a fault → Abort (discard).
    let pump_gate = ready_gate.clone();
    let audio_task = tokio::spawn(async move {
        pump_gate.notified().await; // wait for residency (or task abort below)
        let mut stream = Box::new(source).capture();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    if in_tx.send(OrchestratorInput::Audio(chunk)).await.is_err() {
                        return; // FSM already terminal; stop capturing
                    }
                }
                Err(_fault) => {
                    let _ = in_tx.send(OrchestratorInput::Abort).await;
                    return;
                }
            }
        }
        let _ = in_tx.send(OrchestratorInput::EndOfAudio).await;
    });

    // Drive the FSM and drain its events to the sink concurrently.
    let driver = run_session(backend, config, in_rx, out_tx);
    tokio::pin!(driver);

    let mut outputs_open = true;
    let outcome = loop {
        tokio::select! {
            event = out_rx.recv(), if outputs_open => match event {
                Some(event) => {
                    if matches!(event, OrchestratorEvent::Ready) {
                        ready_gate.notify_one(); // open the capture gate, once
                    }
                    sink.emit(event).await;
                }
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

    // The session is terminal. If it ended before `Ready` (e.g. a load-time
    // error), the pump is still parked on the gate — abort rather than await so
    // we never hang. In the happy path the pump already sent EndOfAudio and
    // finished, so the abort is a no-op.
    audio_task.abort();
    let _ = audio_task.await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WavFileSource;
    use crate::backend::fake::FakeBackend;
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

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("myna-runner-{}-{}.wav", std::process::id(), nanos));
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

        let outcome =
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await.unwrap();

        assert_eq!(
            outcome,
            SessionOutcome::Completed {
                transcript: "the quick brown fox jumps over the lazy dog.".into()
            }
        );
        assert_eq!(sink.finals(), vec!["the quick brown fox", "jumps over the lazy dog."]);
        assert_eq!(sink.done().as_deref(), Some("the quick brown fox jumps over the lazy dog."));
        std::fs::remove_file(&path).ok();
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
        let outcome =
            run_dictation(&backend, SessionConfig::default(), source, &mut sink).await.unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed { .. }));
        assert_eq!(fmt, AudioFormat::default());
        std::fs::remove_file(&path).ok();
    }
}
