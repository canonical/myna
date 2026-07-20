//! Env-gated activity-indicator integration suite (display-present gate).
//!
//! Exercises the real `GtkIndicator` (feature `ui-gtk`) on a session with a
//! display: visible within the activation-latency target (N5, SC-005), AT-SPI
//! exposure (N6), the notification fallback (N7), and the perf watermarks
//! (T040). Populated in branches 003d (T028) and 003f (T040). Skips cleanly
//! when no display is present or the feature is off, so the suite compiles and
//! runs as a no-op offline (Principle II).

/// True when a display is present (`WAYLAND_DISPLAY` / `DISPLAY`). No display →
/// skip.
fn display_present() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

#[test]
fn gate_skips_cleanly_without_display_or_feature() {
    if cfg!(feature = "ui-gtk") && display_present() {
        // Real GtkIndicator assertions land in T028 / T040.
    } else if !cfg!(feature = "ui-gtk") {
        eprintln!("skipping indicator_hw: built without the ui-gtk feature");
    } else {
        eprintln!("skipping indicator_hw: no WAYLAND_DISPLAY / DISPLAY");
    }
}
