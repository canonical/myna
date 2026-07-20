//! `NotifyIndicator` — the headless / error-toast activity indicator (plan T22).
//!
//! Uses `notify-rust` desktop notifications so the controller runs without GTK
//! (the MVP indicator; the persistent GTK overlay is branch 003d/US3). The real
//! notification wiring lands in branch 003b (T019); this foundational branch
//! declares the type so the module tree and binary wiring compile.

use async_trait::async_trait;

use super::{Indicator, IndicatorState};

/// A `notify-rust`-backed indicator (error toasts / headless fallback).
/// Implementation lands in T019.
#[derive(Debug, Default)]
pub struct NotifyIndicator {
    _private: (),
}

impl NotifyIndicator {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Indicator for NotifyIndicator {
    async fn set_state(&mut self, _state: IndicatorState) {}

    async fn hide(&mut self) {}
}
