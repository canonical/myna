//! The session pump both WebSocket dialects share. One task owns the
//! connection and polls an in-flight write alongside the read side, so a peer
//! that stops reading audio cannot hide its errors, and an abort never waits
//! behind queued audio.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use myna_core::TranscriptionEvent;
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use super::{channels, BackendError, BackendEvents, BackendSink, EventSender, Outbound, Outbox};
use crate::task::TaskGuard;

pub(crate) type Ws = WebSocketStream<UnixStream>;

/// How one wire dialect frames a session.
pub(crate) trait Dialect: Send + 'static {
    fn encode(&mut self, item: Outbound) -> Message;

    /// An item finished writing; `finish` says it was the end of audio.
    fn written(&mut self, _finish: bool) {}

    /// Decode one text frame. An `Err` ends the session.
    fn decode(&mut self, text: &str) -> Result<Vec<TranscriptionEvent>, BackendError>;
}

/// Pump `ws` for one session. The task lives as long as the returned events.
pub(crate) fn spawn<D: Dialect>(
    ws: Ws,
    dialect: D,
    outbound_capacity: usize,
    event_capacity: usize,
) -> (BackendSink, BackendEvents) {
    let (sink, outbox, events, ev_tx) = channels(outbound_capacity, event_capacity);
    let (activity, watched) = watch::channel(());
    let task = TaskGuard::spawn(pump(ws, dialect, outbox, ev_tx, activity));
    (sink, events.owning(task).watching(watched))
}

type Writer = SplitSink<Ws, Message>;
type Write = Pin<Box<dyn Future<Output = (Writer, Result<(), WsError>, bool)> + Send>>;

/// How long a session whose write failed keeps reading. A peer that hung up
/// mid-write usually sent its error frame first, and that is already on its
/// way; nothing else can arrive that this session could still act on.
const WRITE_GRACE: Duration = Duration::from_secs(2);

async fn in_flight(write: &mut Option<Write>) -> (Writer, Result<(), WsError>, bool) {
    match write {
        Some(write) => write.await,
        None => std::future::pending().await,
    }
}

/// A write that failed: the session can deliver no more audio, so it reports
/// this unless the peer says something terminal first.
struct Failed {
    error: String,
    grace: Pin<Box<tokio::time::Sleep>>,
}

async fn out_of_grace(failed: &mut Option<Failed>) {
    match failed {
        Some(failed) => (&mut failed.grace).await,
        None => std::future::pending().await,
    }
}

async fn pump<D: Dialect>(
    ws: Ws,
    mut dialect: D,
    mut outbox: Outbox,
    events: EventSender,
    activity: watch::Sender<()>,
) {
    let (writer, mut read) = ws.split();
    let mut idle = Some(writer);
    let mut writing: Option<Write> = None;
    let mut outbound_open = true;
    let mut failed: Option<Failed> = None;
    loop {
        tokio::select! {
            biased;
            // Abort is the socket ending: the owner drops these events right
            // after aborting, which would cancel any close handshake anyway.
            () = outbox.abort.aborted() => {
                myna_core::dbg_log!("ws", "-> abort");
                return;
            }
            (writer, sent, finish) = in_flight(&mut writing), if writing.is_some() => {
                writing = None;
                // A failed write stops writing, not reading: the peer may
                // have sent its error just before hanging up. It gets
                // WRITE_GRACE to arrive, then the write error is the answer -
                // a peer that never closes cannot leave the session waiting
                // on a queue nothing will ever take from again.
                match sent {
                    Ok(()) => {
                        dialect.written(finish);
                        idle = Some(writer);
                    }
                    Err(e) => {
                        myna_core::dbg_log!("ws", "-> write failed: {e}");
                        failed = Some(Failed {
                            error: e.to_string(),
                            grace: Box::pin(tokio::time::sleep(WRITE_GRACE)),
                        });
                    }
                }
            }
            () = out_of_grace(&mut failed), if failed.is_some() => {
                let error = failed.take().expect("a failed write is the arm's precondition").error;
                let _ = events.send(Err(BackendError::Transport(error))).await;
                return;
            }
            item = outbox.queue.recv(), if outbound_open && idle.is_some() => match item {
                Some(item) => {
                    let finish = matches!(item, Outbound::Finish);
                    let message = dialect.encode(item);
                    let mut writer = idle.take().expect("an idle writer is the arm's precondition");
                    writing = Some(Box::pin(async move {
                        let sent = writer.send(message).await;
                        (writer, sent, finish)
                    }));
                }
                // Every sink is gone: stop sending, keep reading (commit-drain).
                None => outbound_open = false,
            },
            incoming = read.next() => {
                // Any data frame shows the backend alive, even one no event
                // comes of; a keepalive ping only shows the socket is.
                if let Some(Ok(Message::Text(_) | Message::Binary(_))) = &incoming {
                    activity.send_replace(());
                }
                match incoming {
                    Some(Ok(Message::Text(text))) => match dialect.decode(&text) {
                        Ok(decoded) => {
                            for event in decoded {
                                let terminal = event.is_terminal();
                                if events.send(Ok(event)).await.is_err() || terminal {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = events.send(Err(e)).await;
                            return;
                        }
                    },
                    // A close before the terminal ends the event stream without
                    // one: the FSM reads that as a failure, never a `done`.
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => {} // binary, ping, pong
                    Some(Err(e)) => {
                        let _ = events.send(Err(BackendError::Transport(e.to_string()))).await;
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myna_core::{AudioFormat, PcmChunk};
    use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

    /// The frames themselves do not matter here, only that they are written.
    struct Frames;

    impl Dialect for Frames {
        fn encode(&mut self, item: Outbound) -> Message {
            match item {
                Outbound::Audio(chunk) => Message::binary(chunk.data),
                Outbound::Finish => Message::text("finish"),
            }
        }

        fn decode(&mut self, _text: &str) -> Result<Vec<TranscriptionEvent>, BackendError> {
            Ok(Vec::new())
        }
    }

    /// A write can fail without the connection ending: here the write buffer
    /// cannot hold the frame, as it cannot when a peer stops reading. The
    /// session can never deliver audio again, so it ends on the write error
    /// instead of leaving the driver gated on a queue nothing will empty.
    #[tokio::test(start_paused = true)]
    async fn a_failed_write_ends_a_session_its_peer_never_answers() {
        let (ours, _theirs) = UnixStream::pair().expect("a socket pair");
        let config = WebSocketConfig::default()
            .write_buffer_size(0)
            .max_write_buffer_size(1024);
        let ws = WebSocketStream::from_raw_socket(ours, Role::Client, Some(config)).await;
        let (sink, mut events) = spawn(ws, Frames, 4, 4);

        let chunk = PcmChunk::new(vec![0u8; 3200], AudioFormat::default());
        sink.reserve()
            .await
            .expect("the session is open")
            .send(Outbound::Audio(chunk));

        // The peer is alive and silent throughout: only the write error can
        // end this, and a session that never ends is the bug, so it is bounded
        // (on a paused clock, WRITE_GRACE comes first or nothing does).
        let ended = tokio::time::timeout(WRITE_GRACE * 30, events.next())
            .await
            .expect("the failed write ends the session");
        match ended {
            Some(Err(BackendError::Transport(_))) => {}
            other => panic!("expected the write error, got {other:?}"),
        }
        assert!(events.next().await.is_none(), "the session is over");
    }
}
