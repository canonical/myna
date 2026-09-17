//! The session pump both WebSocket dialects share. One task owns the
//! connection and polls an in-flight write alongside the read side, so a peer
//! that stops reading audio cannot hide its errors, and an abort never waits
//! behind queued audio.

use std::future::Future;
use std::pin::Pin;

use futures_util::stream::SplitSink;
use futures_util::{FutureExt, SinkExt, StreamExt};
use myna_core::TranscriptionEvent;
use tokio::net::UnixStream;
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
    let task = TaskGuard::spawn(pump(ws, dialect, outbox, ev_tx));
    (sink, events.owning(task))
}

type Writer = SplitSink<Ws, Message>;
type Write = Pin<Box<dyn Future<Output = (Writer, Result<(), WsError>, bool)> + Send>>;

async fn in_flight(write: &mut Option<Write>) -> (Writer, Result<(), WsError>, bool) {
    match write {
        Some(write) => write.await,
        None => std::future::pending().await,
    }
}

async fn pump<D: Dialect>(ws: Ws, mut dialect: D, mut outbox: Outbox, events: EventSender) {
    let (writer, mut read) = ws.split();
    let mut idle = Some(writer);
    let mut writing: Option<Write> = None;
    let mut outbound_open = true;
    loop {
        tokio::select! {
            biased;
            () = outbox.abort.aborted() => {
                myna_core::dbg_log!("ws", "-> abort");
                if let Some(mut writer) = idle.take() {
                    // A close frame only if the socket takes it right away.
                    let _ = writer.close().now_or_never();
                }
                return;
            }
            (writer, sent, finish) = in_flight(&mut writing), if writing.is_some() => {
                writing = None;
                if sent.is_err() {
                    return;
                }
                dialect.written(finish);
                idle = Some(writer);
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
            incoming = read.next() => match incoming {
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
            },
        }
    }
}
