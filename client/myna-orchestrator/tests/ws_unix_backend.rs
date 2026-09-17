//! Integration test for the WS-over-UDS backend (plan T39): spawn the real
//! Python `myna-server --adapter fake` on a Unix socket and drive a full
//! session through [`run_session`] over [`WsUnixBackend`] - handshake, audio
//! push, finish, and the terminal transcript. This is the orchestrator's first
//! real end-to-end round-trip against the running inference infrastructure
//! (the fake adapter standing in for the snap).
//!
//! Every action that runs this suite (`test`, `cov`, `mutants` in
//! `.workshop/myna.yaml`) runs `uv sync` in `server/` first, so the venv
//! binary is always present when this test runs as a gate; a missing binary
//! means the environment is broken and the test fails loudly rather than
//! skipping. The server interaction is wrapped in a bounded timeout so a
//! hung server fails the test instead of hanging the suite.

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
/// as a grandchild that our `kill` couldn't reach, orphaning it).
///
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

/// Bounds every await on the server so a hang fails this test instead of
/// hanging the suite.
const HANG_GUARD: Duration = Duration::from_secs(30);

#[tokio::test]
async fn fake_server_round_trip() {
    let socket = unique_socket_path();
    let _server = spawn_fake_server(&socket);

    if !wait_for_socket(&socket).await {
        panic!("server did not bind {} within timeout", socket.display());
    }

    let backend = WsUnixBackend::new(&socket);
    let handle = tokio::time::timeout(HANG_GUARD, backend.open_session(SessionConfig::default()))
        .await
        .unwrap_or_else(|_| panic!("open_session did not complete within {HANG_GUARD:?}"))
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
    let outcome = tokio::time::timeout(
        HANG_GUARD,
        run_session(&backend, SessionConfig::default(), inputs, control, outputs),
    )
    .await
    .unwrap_or_else(|_| panic!("run_session did not complete within {HANG_GUARD:?}"))
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
