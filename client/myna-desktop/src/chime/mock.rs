//! `MockChimePlayer` — the hermetic [`ChimePlayer`](super::ChimePlayer)
//! fixture. Records the chime sequence so `chime` module tests can assert it
//! with no PipeWire connection and no audio hardware.

use std::sync::{Arc, Mutex};

use super::{Chime, ChimePlayer};

/// A hermetic [`ChimePlayer`] that records every chime it is asked to play.
/// Cheaply `Clone`able (shared log) so a test can hold a handle after handing
/// the player to a [`super::ChimingIndicator`].
#[derive(Clone, Default)]
pub struct MockChimePlayer {
    log: Arc<Mutex<Vec<Chime>>>,
}

impl MockChimePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded chime sequence, in play order.
    pub fn log(&self) -> Vec<Chime> {
        self.log.lock().unwrap().clone()
    }
}

impl ChimePlayer for MockChimePlayer {
    fn play(&mut self, chime: Chime) {
        self.log.lock().unwrap().push(chime);
    }
}
