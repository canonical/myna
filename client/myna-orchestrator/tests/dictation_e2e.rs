//! End-to-end integration test for the T41 dictation chain: spawn the real
//! Python `myna-server --adapter fake` on a Unix socket and run a full utterance
//! through [`run_dictation`] — a [`WavFileSource`] pushing PCM, the FSM driver
//! mediating the ws+unix backend, and a [`CollectingSink`] capturing the result.
//! This is the Rust `dev/dictate.py`, minus the live mic.
//!
//! Every action that runs this suite (`test`, `cov`, `mutants` in
//! `.workshop/myna.yaml`) runs `uv sync` in `server/` first, so the venv
//! binary is always present when this test runs as a gate; a missing binary
//! means the environment is broken and the test fails loudly rather than
//! skipping. The server interaction is wrapped in a bounded timeout so a
//! hung server fails the test instead of hanging the suite. Mirrors
//! `tests/ws_unix_backend.rs` (T39).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use myna_core::{AudioFormat, SessionConfig};
use myna_orchestrator::{run_dictation, CollectingSink, SessionOutcome, WavFileSource};

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The checkout (`<repo>/client/myna-orchestrator` is two levels down).
/// `MYNA_REPO_ROOT` names it when `client/` runs as a copy of its own, which
/// is how cargo-mutants builds; without it the venv server is never found and
/// `spawn_fake_server` panics.
fn repo_root() -> PathBuf {
    std::env::var_os("MYNA_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("repo root is two levels up")
                .to_path_buf()
        })
}

/// Both tests run in parallel in this process, so a clock reading is not an
/// identity: two equal paths put both clients on one server.
fn unique_path(suffix: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "myna-orch-t41-{}-{}.{suffix}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Write a canonical PCM WAV of `seconds` of silence in the default format.
fn write_silence_wav(seconds: usize) -> PathBuf {
    let fmt = AudioFormat::default();
    let data = vec![0u8; fmt.bytes_per_second() as usize * seconds];
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
    let path = unique_path("wav");
    std::fs::write(&path, out).unwrap();
    path
}

/// Panics (does not skip) if the venv binary is missing: every action that
/// runs this suite syncs the venv first (see the module doc), so a missing
/// binary here means `uv sync` was not run - a broken environment, not an
/// environment this test should quietly decline to cover.
fn spawn_fake_server(socket: &Path) -> ServerGuard {
    let server_bin = repo_root().join("server/.venv/bin/myna-server");
    assert!(
        server_bin.exists(),
        "{} not found; run `cd server && uv sync` first (every workshop \
         action that runs this suite does this automatically - see \
         .workshop/myna.yaml)",
        server_bin.display()
    );
    let child = Command::new(&server_bin)
        .args(["--adapter", "fake", "--socket"])
        .arg(socket)
        .current_dir(repo_root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to launch {}: {e}", server_bin.display()));
    ServerGuard(child)
}

async fn wait_for_socket(path: &Path) -> bool {
    for _ in 0..200 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Bounds the await on `run_dictation` so a hung server fails this test
/// instead of hanging the suite.
const HANG_GUARD: Duration = Duration::from_secs(30);

#[tokio::test]
async fn wav_dictation_round_trip_against_real_server() {
    let socket = unique_path("sock");
    let _server = spawn_fake_server(&socket);
    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let wav = write_silence_wav(1);
    let source = WavFileSource::new(&wav).unwrap(); // not realtime — fast test
    let backend = myna_orchestrator::WsUnixBackend::new(&socket);
    let mut sink = CollectingSink::default();

    let outcome = tokio::time::timeout(
        HANG_GUARD,
        run_dictation(&backend, SessionConfig::default(), source, &mut sink),
    )
    .await
    .unwrap_or_else(|_| panic!("run_dictation did not complete within {HANG_GUARD:?}"))
    .expect("session opens against the fake server");

    assert_eq!(
        outcome,
        SessionOutcome::Completed {
            transcript: "The quick brown fox jumps over the lazy dog.".into()
        },
    );
    assert_eq!(
        sink.finals(),
        vec!["The quick brown fox", "jumps over the lazy dog."]
    );

    std::fs::remove_file(&wav).ok();
}

/// The same chain over the IE115 dialect (T43/T47): the server holds the
/// connection open after `completed` (persistent multi-commit shape) and the
/// client treats its commit's `completed` as the terminal `done`, closing its
/// own side — no synthesised done, no reliance on a server close.
#[tokio::test]
async fn wav_dictation_round_trip_over_ie115_dialect() {
    let socket = unique_path("sock");
    let _server = spawn_fake_server(&socket);
    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let wav = write_silence_wav(1);
    let source = WavFileSource::new(&wav).unwrap();
    let backend = myna_orchestrator::WsUnixIe115Backend::new(&socket);
    let mut sink = CollectingSink::default();

    let outcome = tokio::time::timeout(
        HANG_GUARD,
        run_dictation(&backend, SessionConfig::default(), source, &mut sink),
    )
    .await
    .unwrap_or_else(|_| panic!("run_dictation did not complete within {HANG_GUARD:?}"))
    .expect("session opens against the fake server");

    assert_eq!(
        outcome,
        SessionOutcome::Completed {
            transcript: "The quick brown fox jumps over the lazy dog.".into()
        },
    );
    // Committed segments arrive as IE115 deltas and decode back to finals.
    assert_eq!(
        sink.finals(),
        vec!["The quick brown fox", "jumps over the lazy dog."]
    );

    std::fs::remove_file(&wav).ok();
}
