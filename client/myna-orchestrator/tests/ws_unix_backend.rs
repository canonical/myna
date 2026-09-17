//! Integration test for the WS-over-UDS backend (plan T39): spawn the real
//! Python `myna-server --adapter fake` on a Unix socket and drive a full
//! session through [`run_session`] over [`WsUnixBackend`] - handshake, audio
//! push, finish, and the terminal transcript. This is the orchestrator's first
//! real end-to-end round-trip against the running inference infrastructure
//! (the fake adapter standing in for the snap).
//!
//! Skips (passes with a note) if `uv`/`myna-server` can't be launched, so it is
//! robust across environments; when the server *does* start but misbehaves, it
//! fails loudly.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use myna_core::{AudioFormat, PcmChunk, SessionConfig};
use myna_orchestrator::{
    run_session, BackendClient, OrchestratorEvent, OrchestratorInput, SessionOutcome, WsUnixBackend,
};
use tokio::sync::mpsc;

/// Kills the server child on drop so a failed assertion never leaks a process.
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
/// the test skips.
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

/// Tests run in parallel in this process, so a clock reading is not an
/// identity: two equal paths put both clients on one server.
fn unique_socket_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "myna-orch-t39-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Launch `myna-server --adapter fake` on `socket` as a **direct** child (via
/// the project venv binary, not `uv run` — a wrapper would fork the real server
/// as a grandchild that our `kill` couldn't reach, orphaning it). `None` if the
/// venv server isn't built (environment can't run the test — skip).
fn spawn_fake_server(socket: &Path) -> Option<ServerGuard> {
    let server_bin = repo_root().join("server/.venv/bin/myna-server");
    if !server_bin.exists() {
        eprintln!(
            "SKIP: {} not found; run `uv sync` first",
            server_bin.display()
        );
        return None;
    }
    let child = Command::new(&server_bin)
        .args(["--adapter", "fake", "--socket"])
        .arg(socket)
        .current_dir(repo_root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
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
async fn fake_server_round_trip() {
    let socket = unique_socket_path();
    let Some(_server) = spawn_fake_server(&socket) else {
        return; // skip: no uv
    };

    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let backend = WsUnixBackend::new(&socket);
    let handle = backend
        .open_session(SessionConfig::default())
        .await
        .expect("open_session against fake server");
    // Handshake acked the served protocol version.
    assert_eq!(handle.protocol_version(), Some("1"));
    drop(handle);

    // Push a little silence, then signal end-of-audio. The fake adapter's
    // scripted `done` waits for end-of-audio, so this exercises the finish path.
    let fmt = AudioFormat::default();
    let chunk = PcmChunk::new(vec![0u8; fmt.bytes_per_second() as usize / 10], fmt); // ~100 ms
    let (audio, inputs) = mpsc::channel(4);
    let (_control, control) = mpsc::channel(1);
    let (outputs, mut shown) = mpsc::channel(64);
    audio
        .send(OrchestratorInput::Audio(chunk.clone()))
        .await
        .unwrap();
    audio.send(OrchestratorInput::Audio(chunk)).await.unwrap();
    audio.send(OrchestratorInput::EndOfAudio).await.unwrap();
    let outcome = run_session(&backend, SessionConfig::default(), inputs, control, outputs)
        .await
        .expect("session against fake server");

    let mut finals: Vec<String> = Vec::new();
    let mut saw_progress = false;
    while let Some(event) = shown.recv().await {
        match event {
            OrchestratorEvent::Loading
            | OrchestratorEvent::Ready
            | OrchestratorEvent::Transcribing => saw_progress = true,
            OrchestratorEvent::Final(text) => finals.push(text),
            OrchestratorEvent::Error { code, message } => {
                panic!("unexpected error event: {code}: {message}")
            }
            _ => {}
        }
    }

    assert!(saw_progress, "expected at least one progress event");
    assert_eq!(
        finals,
        vec![
            "The quick brown fox".to_string(),
            "jumps over the lazy dog.".to_string()
        ],
        "scripted final segments from the fake adapter",
    );
    assert_eq!(
        outcome,
        SessionOutcome::Completed {
            transcript: "The quick brown fox jumps over the lazy dog.".into()
        },
        "done carries the full aggregated transcript",
    );
}
