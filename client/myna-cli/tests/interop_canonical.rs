//! Interop integration test against the canonical/whisper-snap adapter (T061).
//!
//! Verifies that our IE115 client can complete a full session against the
//! colleagues' adapter (Go `whisperlive-adapter` + WhisperLive docker backend):
//! connect → base64 audio → deltas → completed. This is the SC-006 fixture.
//!
//! Gated on the adapter socket being present — skipped otherwise (the adapter
//! + docker backend are a manual lab fixture, not CI infra):
//!
//! ```sh
//! sudo docker run --rm -p 9090:9090 ghcr.io/collabora/whisperlive-cpu:latest
//! go run ./cmd/whisperlive-adapter serve --unix-socket /tmp/myna-adapter.sock ...
//! cargo test -p myna-cli --test interop_canonical -- --ignored
//! ```
//!
//! The clip must hold a single utterance: their `completed` fires per VAD
//! segment, not per commit, so a pause ends the session early (2026-08-20,
//! `docs/interop/canonical-whisper-snap-report.md`).

use std::path::PathBuf;

const ADAPTER_SOCKET: &str = "/tmp/myna-adapter.sock";

fn clip(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("corpus/real/audio")
        .join(name);
    p.exists().then_some(p)
}

/// Full round-trip against the live adapter: the session must complete with a
/// non-empty transcript. Deltas from their backend arrive without a
/// `disposition` field (backward-compat → committed), which today restates the
/// growing hypothesis — the finding documented in
/// `docs/interop/canonical-whisper-snap-report.md`. This test pins the
/// session-level contract (completion, terminal done) regardless of how many
/// deltas precede it.
#[tokio::test]
#[ignore = "requires the canonical/whisper-snap adapter + WhisperLive docker backend"]
async fn session_against_canonical_adapter_completes() {
    use myna_orchestrator::myna_core::SessionConfig;
    use myna_orchestrator::{
        run_dictation, CollectingSink, SessionOutcome, WavFileSource, WsUnixIe115Backend,
    };

    let socket = PathBuf::from(ADAPTER_SOCKET);
    if !socket.exists() {
        eprintln!("adapter socket {ADAPTER_SOCKET} absent — skipping");
        return;
    }
    let clip = clip("librispeech-2277-149896-0005.wav").expect("real corpus clip missing");

    let backend = WsUnixIe115Backend::new(&socket)
        .base64_audio(true)
        .ws_path("/v1/realtime");
    let source = WavFileSource::new(&clip).unwrap();
    let mut sink = CollectingSink::default();

    let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
        .await
        .expect("session transport failed");

    let SessionOutcome::Completed { transcript } = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let normalized = transcript.trim().to_lowercase();
    assert!(
        normalized.contains("wrinkles"),
        "unexpected transcript: {transcript}"
    );
    // The terminal done must be non-empty (their empty-completed-as-reset is
    // handled as a non-terminal, gap #3).
    assert_eq!(
        sink.done().as_deref().map(str::trim),
        Some(transcript.trim())
    );
}
