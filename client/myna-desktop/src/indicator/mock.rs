//! `MockIndicator` — the hermetic activity-indicator fixture (T008).
//!
//! Records the `IndicatorState` sequence (and `hide()`) so controller tests can
//! assert the state timeline (Recording→Transcribing→Finalizing→Hidden, error
//! states) with no GTK and no display. Built without the `ui-gtk` feature
//! (contract indicator.md N-mapping tests).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{Indicator, IndicatorState};

/// A hermetic [`Indicator`] that records every state it is shown. Clone the log
/// handle (`.log()`) before moving the mock into the controller.
#[derive(Default)]
pub struct MockIndicator {
    log: Arc<Mutex<Vec<IndicatorState>>>,
}

impl MockIndicator {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle to the recorded state sequence.
    pub fn log(&self) -> Arc<Mutex<Vec<IndicatorState>>> {
        self.log.clone()
    }
}

#[async_trait]
impl Indicator for MockIndicator {
    async fn set_state(&mut self, state: IndicatorState) {
        self.log.lock().unwrap().push(state);
    }

    async fn hide(&mut self) {
        self.log.lock().unwrap().push(IndicatorState::Hidden);
    }
}
