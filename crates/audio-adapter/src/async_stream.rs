use crate::error::Error;
use crate::frame::StreamItem;
use crate::AudioStream;
use futures_core::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Async stream adapter over an `AudioStream`.
///
/// Polling never blocks the executor: each poll drains whatever is buffered
/// and, when nothing is available, immediately schedules a re-poll via the
/// waker. This trades some idle wakeups for correctness; a notification-driven
/// waker can replace it without changing the public type.
pub struct AudioStreamAdapter {
    inner: AudioStream,
    pending: VecDeque<StreamItem>,
}

impl AudioStreamAdapter {
    pub fn new(stream: AudioStream) -> Self {
        Self {
            inner: stream,
            pending: VecDeque::new(),
        }
    }
}

impl Stream for AudioStreamAdapter {
    type Item = Result<StreamItem, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let adapter = self.get_mut();
        if let Some(item) = adapter.pending.pop_front() {
            return Poll::Ready(Some(Ok(item)));
        }
        match adapter.inner.read() {
            Ok(items) if !items.is_empty() => {
                let mut iter = items.into_iter();
                let first = iter.next().expect("non-empty");
                adapter.pending.extend(iter);
                Poll::Ready(Some(Ok(first)))
            }
            Ok(_) => {
                if adapter.inner.is_closed() {
                    return Poll::Ready(None);
                }
                // Nothing buffered yet: request a re-poll rather than
                // returning Pending without a registered waker (which would
                // park the task forever).
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

impl AudioStream {
    /// Convert into a `futures::Stream` of items.
    pub fn into_stream(self) -> AudioStreamAdapter {
        AudioStreamAdapter::new(self)
    }
}
