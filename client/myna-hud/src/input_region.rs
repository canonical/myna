//! input_region — PURE per-state input-region geometry (R22, FR-025, SC-015).
//!
//! The pill's window declares its own input region (GDK's
//! `Surface::set_input_region`, Wayland `wl_surface_set_input_region`): an
//! **empty region** — fully click-through — in every state, with the sole
//! exception of a critical error, where the region covers exactly the
//! dismiss (×) control's rectangle so FR-007b's explicit dismiss still works.
//! The region is re-applied on map and after size-allocate (the toolkit can
//! invalidate it).
//!
//! The compositor/extension host never touches input: the client-side region
//! is the platform's own mechanism for this exact use, and the mutter-side
//! override is not introspectable extension API anyway.
//!
//! This module is the pure decision (state → rects); the window applies it.

use crate::states::Severity;

/// A rectangle in surface-local coordinates (px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The interactive rectangles for the current state:
/// - **Any non-critical state** (idle, loading, recording, transcribing,
///   finalizing, notice, active): *none* — the window is fully
///   click-through; pointer events pass to the application underneath.
/// - **Critical error**: exactly the dismiss control's rectangle, when its
///   layout is known (`Some`); still empty before layout so nothing is
///   accidentally interactive.
///
/// `dismiss_allocation` is the dismiss (×) control's current
/// surface-local rectangle as computed by the window's layout (`None` until
/// laid out or while the control is not shown).
pub fn input_region_rects(
    severity: Option<Severity>,
    dismiss_allocation: Option<Rect>,
) -> Vec<Rect> {
    match severity {
        Some(Severity::Critical) => dismiss_allocation.into_iter().collect(),
        _ => Vec::new(),
    }
}
