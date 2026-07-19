//! End-to-end integration test for the T41 dictation chain: spawn the real
//! Python `myna-server --adapter fake` on a Unix socket and run a full utterance
//! through [`run_dictation`] — a [`WavFileSource`] pushing PCM, the FSM driver
//! mediating the ws+unix backend, and a [`CollectingSink`] capturing the result.
//! This is the Rust `dev/dictate.py`, minus the live mic.
//!
//! Skips (passes with a note) if the venv `myna-server` isn't built, so it is
//! robust across environments; when the server starts but misbehaves, it fails
//! loudly. Mirrors `tests/ws_unix_backend.rs` (T39).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use myna_core::{AudioFormat, SessionConfig};
use myna_orchestrator::{run_dictation, CollectingSink, SessionOutcome, WavFileSource};

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels up")
        .to_path_buf()
}

fn unique_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("myna-orch-t41-{}-{}.{suffix}", std::process::id(), nanos))
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

fn spawn_fake_server(socket: &Path) -> Option<ServerGuard> {
    let server_bin = repo_root().join(".venv/bin/myna-server");
    if !server_bin.exists() {
        eprintln!("SKIP: {} not found; run `uv sync` first", server_bin.display());
        return None;
    }
    match Command::new(&server_bin)
        .args(["--adapter", "fake", "--socket"])
        .arg(socket)
        .current_dir(repo_root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => Some(ServerGuard(c)),
        Err(e) => {
            eprintln!("SKIP: cannot launch {} ({e})", server_bin.display());
            None
        }
    }
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

#[tokio::test]
async fn wav_dictation_round_trip_against_real_server() {
    let socket = unique_path("sock");
    let Some(_server) = spawn_fake_server(&socket) else {
        return; // skip: no venv server
    };
    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let wav = write_silence_wav(1);
    let source = WavFileSource::new(&wav).unwrap(); // not realtime — fast test
    let backend = myna_orchestrator::WsUnixBackend::new(&socket);
    let mut sink = CollectingSink::default();

    let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
        .await
        .expect("session opens against the fake server");

    assert_eq!(
        outcome,
        SessionOutcome::Completed {
            transcript: "The quick brown fox jumps over the lazy dog.".into()
        },
    );
    assert_eq!(sink.finals(), vec!["The quick brown fox", "jumps over the lazy dog."]);

    std::fs::remove_file(&wav).ok();
}

/// The same chain over the IE115 dialect (T43/T47): the server holds the
/// connection open after `completed` (persistent multi-commit shape) and the
/// client treats its commit's `completed` as the terminal `done`, closing its
/// own side — no synthesised done, no reliance on a server close.
#[tokio::test]
async fn wav_dictation_round_trip_over_ie115_dialect() {
    let socket = unique_path("sock");
    let Some(_server) = spawn_fake_server(&socket) else {
        return; // skip: no venv server
    };
    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let wav = write_silence_wav(1);
    let source = WavFileSource::new(&wav).unwrap();
    let backend = myna_orchestrator::WsUnixIe115Backend::new(&socket);
    let mut sink = CollectingSink::default();

    let outcome = run_dictation(&backend, SessionConfig::default(), source, &mut sink)
        .await
        .expect("session opens against the fake server");

    assert_eq!(
        outcome,
        SessionOutcome::Completed {
            transcript: "The quick brown fox jumps over the lazy dog.".into()
        },
    );
    // Committed segments arrive as IE115 deltas and decode back to finals.
    assert_eq!(sink.finals(), vec!["The quick brown fox", "jumps over the lazy dog."]);

    std::fs::remove_file(&wav).ok();
}
