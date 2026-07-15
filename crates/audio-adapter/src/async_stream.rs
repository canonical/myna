use crate::error::Error;
use crate::frame::StreamItem;
use crate::AudioStream;
use futures_core::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// Async stream adapter over an `AudioStream`.
pub struct AudioStreamAdapter {
    inner: AudioStream,
    timeout: Duration,
    pending: VecDeque<StreamItem>,
}

impl AudioStreamAdapter {
    pub fn new(stream: AudioStream, timeout: Duration) -> Self {
        Self {
            inner: stream,
            timeout,
            pending: VecDeque::new(),
        }
    }
}

impl Stream for AudioStreamAdapter {
    type Item = Result<StreamItem, Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let adapter = self.get_mut();
        if let Some(item) = adapter.pending.pop_front() {
            return Poll::Ready(Some(Ok(item)));
        }
        match adapter.inner.read_timeout(adapter.timeout) {
            Ok(items) => {
                if items.is_empty() {
                    Poll::Pending
                } else {
                    let mut iter = items.into_iter();
                    if let Some(first) = iter.next() {
                        adapter.pending.extend(iter);
                        Poll::Ready(Some(Ok(first)))
                    } else {
                        Poll::Pending
                    }
                }
            }
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

impl AudioStream {
    /// Convert into a `futures::Stream`.
    pub fn into_stream(self, timeout: Duration) -> AudioStreamAdapter {
        AudioStreamAdapter::new(self, timeout)
    }
}
