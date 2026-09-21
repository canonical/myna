//! Child tasks owned by the future that spawned them.

use std::future::Future;

use tokio::task::JoinHandle;

/// A spawned task that is aborted when its guard drops, so an owner that is
/// itself cancelled never leaves the task running detached.
pub(crate) struct TaskGuard(JoinHandle<()>);

impl TaskGuard {
    pub(crate) fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self(tokio::spawn(future))
    }

    /// Abort the task and wait until it has stopped.
    pub(crate) async fn cancel(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
