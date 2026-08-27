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
//!
//! **Amended 2026-08-26**: the region is now empty in every state. It
//! previously punched a hole for a critical error's dismiss (×) control,
//! which no longer exists — the HUD receives no events, and the client
//! clears an error by publishing a new state.

use crate::states::Severity;

/// A rectangle in surface-local coordinates (px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The interactive rectangles for the current state: **none, in every
/// state** (FR-025).
///
/// The HUD takes no pointer input at all. It is an overlay that reports
/// what the dictation session is doing; it is never a target, and it
/// carries no control to click — a critical error is cleared by the client
/// publishing a new state, not by the user dismissing it.
///
/// The severity is still taken, so the call site says which state is being
/// made click-through and the test below can assert that the answer does
/// not vary with it.
pub fn input_region_rects(_severity: Option<Severity>) -> Vec<Rect> {
    Vec::new()
}
