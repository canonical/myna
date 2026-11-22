//! Publishes whether the chosen processing chain can do all it asks.
//!
//! `ChainStatus` is `ready`, `degraded` (some model step has no installed
//! model and is skipped) or `restart` (a model was connected after this
//! daemon started, so only a fresh process sees it); `ChainMissing` lists
//! the uncovered tasks. Settings reads both to tell the user, so a chain
//! that quietly does nothing is never mistaken for a chain that works.

use tokio::sync::watch;

use crate::dbus::{PropertyValue, SharedBus};
use crate::process::resolve::ChainStatus;

pub const READY: &str = "ready";
const DEGRADED: &str = "degraded";
const RESTART: &str = "restart";

/// The two property values for `status`.
pub fn wire(status: &ChainStatus) -> (&'static str, String) {
    match status {
        ChainStatus::Ready => (READY, String::new()),
        ChainStatus::Degraded {
            missing_tasks,
            needs_restart,
        } => (
            if *needs_restart { RESTART } else { DEGRADED },
            missing_tasks.join(","),
        ),
    }
}

/// Publish the current status, then every change, until the sender drops.
pub async fn run(bus: SharedBus, mut statuses: watch::Receiver<ChainStatus>) {
    loop {
        let (status, missing) = wire(&statuses.borrow_and_update());
        let mut bus_guard = bus.lock().await;
        bus_guard
            .set_property("ChainStatus", PropertyValue::Str(status.into()))
            .await;
        bus_guard
            .set_property("ChainMissing", PropertyValue::Str(missing))
            .await;
        drop(bus_guard);
        if statuses.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_is_ready_degraded_or_waiting_for_a_restart() {
        assert_eq!(wire(&ChainStatus::Ready), ("ready", String::new()));
        let tasks = vec!["text.cleanup".to_owned(), "text.translate".to_owned()];
        let degraded = ChainStatus::Degraded {
            missing_tasks: tasks.clone(),
            needs_restart: false,
        };
        assert_eq!(
            wire(&degraded),
            ("degraded", "text.cleanup,text.translate".to_owned())
        );
        let restart = ChainStatus::Degraded {
            missing_tasks: tasks,
            needs_restart: true,
        };
        assert_eq!(wire(&restart).0, "restart");
    }
}
