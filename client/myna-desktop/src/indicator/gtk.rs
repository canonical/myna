//! `GtkIndicator` — the persistent GTK4 activity-overlay indicator (plan T22,
//! branch 003d/US3).
//!
//! A borderless, always-on-top, non-focusable GTK4 overlay with distinct visuals
//! per [`IndicatorState`], AT-SPI-labelled for a11y. Gated behind the `ui-gtk`
//! Cargo feature so the hermetic suite never links GTK. The real overlay lands
//! in branch 003d (T029); this foundational branch declares the type so the
//! `ui-gtk` build compiles.

use async_trait::async_trait;

use super::{Indicator, IndicatorState};

/// A GTK4 overlay-window indicator (implementation lands in T029).
#[derive(Debug, Default)]
pub struct GtkIndicator {
    _private: (),
}

impl GtkIndicator {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Indicator for GtkIndicator {
    async fn set_state(&mut self, _state: IndicatorState) {}

    async fn hide(&mut self) {}
}
