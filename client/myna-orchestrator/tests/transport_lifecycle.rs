//! Capture and transport lifecycle through the real socket transports: a
//! scripted capture source, `run_dictation`, a `WsUnix*Backend` and an
//! in-process WebSocket server on a Unix socket whose behaviour each test
//! scripts (stays loading, stops reading, errors while congested).
//!
//! Waits are bounded: a hang fails the test instead of stalling the suite.

use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use myna_audio::{CaptureSource, ScriptedBackend, Step};
use myna_core::{
    AudioFormat, AudioSource, CaptureHealth, ErrorData, Progress, SessionConfig,
    TranscriptionEvent, TranscriptionFinal, PHASE_PREPARING, PHASE_READY,
};
use myna_orchestrator::{
    run_dictation, BackendClient, BackendError, BackendHandle, CollectingSink, SessionOutcome,
    WsUnixBackend, WsUnixIe115Backend,
};
use serde_json::{json, Value};
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const BOUND: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Dialect {
    Internal,
    Ie115,
}

const DIALECTS: [Dialect; 2] = [Dialect::Internal, Dialect::Ie115];

struct Server {
    listener: UnixListener,
    path: PathBuf,
    dialect: Dialect,
}

impl Drop for Server {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

impl Server {
    fn bind(dialect: Dialect) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "myna-transport-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path).unwrap();
        Self {
            listener,
            path,
            dialect,
        }
    }

    /// Accept one client and answer its opening frame.
    async fn accept(&self) -> Conn {
        self.try_accept().await.expect("the client opens a session")
    }

    /// Like [`Server::accept`], but `None` when the client gives up first.
    async fn try_accept(&self) -> Option<Conn> {
        let (stream, _) = self.listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.ok()?;
        let mut conn = Conn {
            ws,
            dialect: self.dialect,
        };
        // Both dialects open with one text frame (`session.start` or
        // `session.update`); only the internal one is acknowledged.
        let Some(Ok(Message::Text(_))) = conn.ws.next().await else {
            return None;
        };
        if let Dialect::Internal = self.dialect {
            conn.send_json(json!({"type": "session.created", "protocol_version": "1"}))
                .await;
        }
        Some(conn)
    }

    async fn open(&self) -> BackendHandle {
        let config = SessionConfig::default();
        match self.dialect {
            Dialect::Internal => WsUnixBackend::new(&self.path).open_session(config).await,
            Dialect::Ie115 => {
                WsUnixIe115Backend::new(&self.path)
                    .open_session(config)
                    .await
            }
        }
        .expect("the session opens")
    }

    async fn run(
        &self,
        source: CaptureSource,
        sink: &mut CollectingSink,
    ) -> Result<SessionOutcome, BackendError> {
        match self.dialect {
            Dialect::Internal => {
                let backend = WsUnixBackend::new(&self.path);
                run_dictation(&backend, SessionConfig::default(), source, sink).await
            }
            Dialect::Ie115 => {
                let backend = WsUnixIe115Backend::new(&self.path);
                run_dictation(&backend, SessionConfig::default(), source, sink).await
            }
        }
    }
}

/// What a server read off one connection until the client closed it.
#[derive(Debug, Default)]
struct Received {
    audio_bytes: usize,
    finished: bool,
}

struct Conn {
    ws: WebSocketStream<UnixStream>,
    dialect: Dialect,
}

impl Conn {
    /// Send a frame. A client that already left is the test's to judge, by
    /// its outcome.
    async fn send_json(&mut self, value: Value) {
        let _ = self.ws.send(Message::text(value.to_string())).await;
    }

    async fn send_event(&mut self, event: TranscriptionEvent) {
        let frame = match (self.dialect, &event) {
            (Dialect::Internal, _) => event.to_wire(),
            (Dialect::Ie115, TranscriptionEvent::Progress(p)) => {
                let state = if p.phase == PHASE_PREPARING {
                    "loading"
                } else {
                    "ready"
                };
                json!({"type": "status", "state": state})
            }
            (Dialect::Ie115, TranscriptionEvent::Error(e)) => {
                json!({"type": "error", "error": {"code": e.code, "message": e.message}})
            }
            (Dialect::Ie115, TranscriptionEvent::Done(d)) => json!({
                "type": "conversation.item.input_audio_transcription.completed",
                "item_id": "i1", "content_index": 0, "transcript": d.text
            }),
            (Dialect::Ie115, other) => panic!("unscripted IE115 event {other:?}"),
        };
        self.send_json(frame).await;
    }

    async fn loading(&mut self) {
        self.send_event(TranscriptionEvent::Progress(Progress::phase(
            PHASE_PREPARING,
        )))
        .await;
    }

    async fn ready(&mut self) {
        self.send_event(TranscriptionEvent::Progress(Progress::phase(PHASE_READY)))
            .await;
    }

    async fn error(&mut self, code: &str) {
        self.send_event(TranscriptionEvent::Error(ErrorData {
            code: code.into(),
            message: "the server gave up".into(),
        }))
        .await;
    }

    async fn done(&mut self, text: &str) {
        self.send_event(TranscriptionEvent::Done(TranscriptionFinal {
            text: text.into(),
            ..Default::default()
        }))
        .await;
    }

    /// Read until the client closes the connection or `until_finish` sees
    /// the end-of-audio frame.
    async fn read(&mut self, until_finish: bool) -> Received {
        let mut received = Received::default();
        while let Some(Ok(message)) = self.ws.next().await {
            match message {
                Message::Binary(data) => received.audio_bytes += data.len(),
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    let kind = value["type"].as_str().unwrap_or_default();
                    if kind == "session.finish" || kind == "input_audio_buffer.commit" {
                        received.finished = true;
                        if until_finish {
                            return received;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        received
    }

    /// Wait until the client is stuck mid-write: part of an audio frame
    /// larger than its socket can ever queue has arrived unread, so the rest
    /// cannot follow until this end reads.
    async fn await_client_blocked(&self) {
        let fd = self.ws.get_ref().as_raw_fd();
        while queued_bytes(fd) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Bytes waiting in a socket's receive queue.
fn queued_bytes(fd: RawFd) -> usize {
    let mut queued: libc::c_int = 0;
    // SAFETY: FIONREAD writes one int through the pointer on a valid fd.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) };
    assert_eq!(rc, 0, "FIONREAD failed");
    queued as usize
}

/// The send buffer a fresh Unix stream socket gets. A writer blocks once its
/// unread bytes reach about this much, so no larger frame fits in one go.
fn socket_send_buffer() -> usize {
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut size: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: SO_SNDBUF writes one int of `len` bytes on a valid fd.
    let rc = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&mut size as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    assert_eq!(rc, 0, "SO_SNDBUF failed");
    size as usize
}

fn source(backend: ScriptedBackend) -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(backend))
        .build()
}

/// A microphone that delivers two chunks at once, then stays open. Each chunk
/// is several socket buffers long, so a server that stops reading leaves the
/// client blocked inside its first audio write.
fn congesting_mic() -> (CaptureSource, Arc<AtomicBool>) {
    let format = AudioFormat::default();
    let bytes = (4 * socket_send_buffer()) as f64;
    let chunk = Duration::from_secs_f64(bytes / format.bytes_per_second() as f64);
    let backend = ScriptedBackend::new(vec![
        Step::Silence(chunk * 2),
        Step::Wait(Duration::from_secs(600)),
    ]);
    let finished = backend.finished();
    let mic = CaptureSource::builder(format)
        .chunk(chunk)
        .backend(Box::new(backend))
        .build();
    (mic, finished)
}

/// A microphone that delivers one second of audio, then stays open.
fn open_mic() -> (CaptureSource, Arc<AtomicBool>) {
    let backend = ScriptedBackend::new(vec![
        Step::Silence(Duration::from_secs(1)),
        Step::Wait(Duration::from_secs(600)),
    ]);
    let finished = backend.finished();
    (source(backend), finished)
}

async fn bounded<F: std::future::Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen within {BOUND:?}"))
}

fn failed_with(outcome: Result<SessionOutcome, BackendError>) -> (String, String) {
    match outcome {
        Ok(SessionOutcome::Failed { code, message }) => (code, message),
        other => panic!("expected a failed session, got {other:?}"),
    }
}

#[tokio::test]
async fn microphone_open_failure_is_reported_while_the_backend_is_loading() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        let mic = source(ScriptedBackend::unavailable("no microphone here"));
        let mut sink = CollectingSink::default();
        // The client may give up before the session is even open.
        let serve = async {
            let Some(mut conn) = server.try_accept().await else {
                return Received::default();
            };
            conn.loading().await;
            conn.read(false).await
        };
        let (outcome, received) = bounded(
            "the open failure surfacing",
            futures_util::future::join(server.run(mic, &mut sink), serve),
        )
        .await;
        let (code, message) = failed_with(outcome);
        assert_eq!(code, "capture_failed", "{dialect:?}");
        assert!(
            message.contains("no microphone here"),
            "{dialect:?}: {message}"
        );
        assert_eq!(received.audio_bytes, 0, "{dialect:?}");
    }
}

#[tokio::test]
async fn capture_overload_before_ready_fails_without_sending_audio() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        let mic = CaptureSource::builder(AudioFormat::default())
            .ring_depth(Duration::from_secs(1))
            .backend(Box::new(ScriptedBackend::new(vec![
                Step::Silence(Duration::from_secs(3)),
                Step::Wait(Duration::from_secs(600)),
            ])))
            .build();
        let mut sink = CollectingSink::default();
        // The client may give up before the session is even open.
        let serve = async {
            let Some(mut conn) = server.try_accept().await else {
                return Received::default();
            };
            conn.loading().await;
            conn.read(false).await
        };
        let (outcome, received) = bounded(
            "the overload surfacing",
            futures_util::future::join(server.run(mic, &mut sink), serve),
        )
        .await;
        let (code, message) = failed_with(outcome);
        assert_eq!(code, "capture_failed", "{dialect:?}");
        assert!(message.contains("overflow"), "{dialect:?}: {message}");
        assert_eq!(
            received.audio_bytes, 0,
            "{dialect:?}: audio sent before ready"
        );
    }
}

#[tokio::test]
async fn backend_error_is_seen_while_outbound_audio_is_backpressured() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        let (mic, _) = congesting_mic();
        let mut sink = CollectingSink::default();
        let serve = async {
            let mut conn = server.accept().await;
            conn.ready().await;
            conn.await_client_blocked().await;
            conn.error("server_stalled").await;
            conn
        };
        let (outcome, _conn) = bounded(
            "the error surfacing under backpressure",
            futures_util::future::join(server.run(mic, &mut sink), serve),
        )
        .await;
        assert_eq!(failed_with(outcome).0, "server_stalled", "{dialect:?}");
    }
}

/// Drop the runner once `ready_to_drop` resolves, then require the capture
/// device released and the connection closed.
async fn dropping_the_runner_releases_everything(dialect: Dialect, backend_ready: bool) {
    let server = Server::bind(dialect);
    let (mic, finished) = if backend_ready {
        congesting_mic()
    } else {
        open_mic()
    };
    let mut health = mic.health();
    let mut sink = CollectingSink::default();
    let mut run = Box::pin(server.run(mic, &mut sink));

    let mut conn = tokio::select! {
        outcome = &mut run => panic!("{dialect:?}: session ended early: {outcome:?}"),
        conn = bounded("the session opening", async {
            let mut conn = server.accept().await;
            if backend_ready {
                conn.ready().await;
                conn.await_client_blocked().await;
            } else {
                conn.loading().await;
                while health.next().await != Some(CaptureHealth::Capturing) {}
            }
            conn
        }) => conn,
    };
    drop(run);

    bounded("the connection closing", conn.read(false)).await;
    bounded("the capture device being released", async {
        while health.next().await.is_some() {}
    })
    .await;
    assert!(finished.load(Ordering::Acquire), "{dialect:?}");
}

#[tokio::test]
async fn dropping_the_runner_before_ready_releases_capture_and_connection() {
    for dialect in DIALECTS {
        dropping_the_runner_releases_everything(dialect, false).await;
    }
}

#[tokio::test]
async fn dropping_the_runner_under_backpressure_releases_capture_and_connection() {
    for dialect in DIALECTS {
        dropping_the_runner_releases_everything(dialect, true).await;
    }
}

#[tokio::test]
async fn release_before_ready_sends_the_whole_prefix_then_finishes() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        // Two seconds of audio, then the capture ends as a release does.
        let backend = ScriptedBackend::new(vec![Step::Silence(Duration::from_secs(2))]);
        let mic = source(backend);
        let mut health = mic.health();
        let mut sink = CollectingSink::default();
        let serve = async {
            let mut conn = server.accept().await;
            conn.loading().await;
            while health.next().await != Some(CaptureHealth::Ended) {}
            conn.ready().await;
            let received = conn.read(true).await;
            conn.done("ok").await;
            received
        };
        let (outcome, received) = bounded(
            "the released utterance completing",
            futures_util::future::join(server.run(mic, &mut sink), serve),
        )
        .await;
        assert!(
            matches!(outcome, Ok(SessionOutcome::Completed { .. })),
            "{dialect:?}: {outcome:?}"
        );
        assert_eq!(received.audio_bytes, 64_000, "{dialect:?}");
        assert!(received.finished, "{dialect:?}");
    }
}

#[tokio::test]
async fn abort_closes_the_connection_while_events_are_still_held() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        let (mut handle, mut conn) =
            futures_util::future::join(server.open(), server.accept()).await;
        handle.sink.abort();
        bounded("the connection closing", conn.read(false)).await;
        let next = bounded("the event stream ending", handle.events.next()).await;
        assert!(next.is_none(), "{dialect:?}: {next:?}");
    }
}

#[tokio::test]
async fn a_malformed_frame_is_an_error_that_ends_the_events() {
    for dialect in DIALECTS {
        let server = Server::bind(dialect);
        let (mut handle, mut conn) =
            futures_util::future::join(server.open(), server.accept()).await;
        conn.ws.send(Message::text("not json")).await.unwrap();
        let first = bounded("the error", handle.events.next()).await;
        assert!(matches!(first, Some(Err(_))), "{dialect:?}: {first:?}");
        let next = bounded("the event stream ending", handle.events.next()).await;
        assert!(next.is_none(), "{dialect:?}: {next:?}");
    }
}
