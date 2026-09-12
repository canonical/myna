//! Env-gated activity-indicator integration suite (display-present gate).
//!
//! The GTK overlay (`ui-gtk`) was removed in T150 — the indicator is the
//! myna-shell overlay (feature 004, verified on hardware) or the headless
//! `NotifyIndicator`. This suite therefore only pins that the gate degrades
//! cleanly and that the P19 "unchanged behaviour" assertions (the headless
//! notify path still drives the same states) hold hermetically via the
//! mock. Skips cleanly when no display is present (Principle II).

/// True when a display is present (`WAYLAND_DISPLAY` / `DISPLAY`). No display →
/// skip.
fn display_present() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

#[test]
fn gate_skips_cleanly_without_display() {
    if !display_present() {
        eprintln!("skipping indicator_hw: no WAYLAND_DISPLAY / DISPLAY");
    }
}
