//! Integration test for the WS-over-UDS backend (plan T39): spawn the real
//! Python `myna-server --adapter fake` on a Unix socket and drive a full
//! session through [`WsUnixBackend`] — handshake, audio push, finish, and the
//! terminal transcript. This is the orchestrator's first real end-to-end
//! round-trip against the running inference infrastructure (the fake adapter
//! standing in for the snap).
//!
//! Skips (passes with a note) if `uv`/`myna-server` can't be launched, so it is
//! robust across environments; when the server *does* start but misbehaves, it
//! fails loudly.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use myna_core::{AudioFormat, PcmChunk, SessionConfig, TranscriptionEvent};
use myna_orchestrator::{BackendClient, WsUnixBackend};

/// Kills the server child on drop so a failed assertion never leaks a process.
struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/rust/myna-orchestrator
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels up")
        .to_path_buf()
}

fn unique_socket_path() -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("myna-orch-t39-{}-{}.sock", std::process::id(), nanos))
}

/// Launch `myna-server --adapter fake` on `socket` as a **direct** child (via
/// the project venv binary, not `uv run` — a wrapper would fork the real server
/// as a grandchild that our `kill` couldn't reach, orphaning it). `None` if the
/// venv server isn't built (environment can't run the test — skip).
fn spawn_fake_server(socket: &Path) -> Option<ServerGuard> {
    let server_bin = repo_root().join(".venv/bin/myna-server");
    if !server_bin.exists() {
        eprintln!("SKIP: {} not found; run `uv sync` first", server_bin.display());
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
    let mut handle = backend
        .open_session(SessionConfig::default())
        .await
        .expect("open_session against fake server");

    // Handshake acked the served protocol version.
    assert_eq!(handle.protocol_version(), Some("1"));

    // Push a little silence, then signal end-of-audio. The fake adapter's
    // scripted `done` waits for end-of-audio, so this exercises the finish path.
    let fmt = AudioFormat::default();
    let chunk = PcmChunk::new(vec![0u8; fmt.bytes_per_second() as usize / 10], fmt); // ~100 ms
    handle.sink.send_audio(chunk.clone()).await.expect("send audio");
    handle.sink.send_audio(chunk).await.expect("send audio");
    handle.sink.finish().await.expect("finish");

    // Drain events to the terminal one.
    let mut finals: Vec<String> = Vec::new();
    let mut saw_progress = false;
    let mut done_text: Option<String> = None;
    while let Some(event) = handle.events.next().await {
        match event.expect("event decode") {
            TranscriptionEvent::Progress(_) => saw_progress = true,
            TranscriptionEvent::Final(f) => finals.push(f.text),
            TranscriptionEvent::Done(d) => {
                done_text = Some(d.text);
                break;
            }
            TranscriptionEvent::Error(e) => panic!("unexpected error event: {e:?}"),
        }
    }

    assert!(saw_progress, "expected at least one progress event");
    assert_eq!(
        finals,
        vec!["The quick brown fox".to_string(), "jumps over the lazy dog.".to_string()],
        "scripted final segments from the fake adapter",
    );
    assert_eq!(
        done_text.as_deref(),
        Some("The quick brown fox jumps over the lazy dog."),
        "done carries the full aggregated transcript",
    );
}
