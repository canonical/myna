//! Client-pipeline instrumentation, in two tiers.
//!
//! **Operational** ([`info`] / `info_log!`) is always on. It records the
//! lifecycle facts that answer "is this daemon the one doing the work?" -
//! shortcut bound, activation edge seen, bus name acquired, backend connected,
//! utterance outcome. It carries no transcript text, so it costs nothing
//! against the no-logging-transcripts-by-default invariant.
//!
//! **Debug** ([`log`] / `dbg_log!`) is gated on `MYNA_DEBUG=1` and streams the
//! whole capture → transport → inject path: byte counts, every liveness event,
//! and the committed text itself. This is the tool for "where did my utterance
//! go?"; it *does* print transcripts (you asked for it explicitly).
//!
//! Both tiers go to stderr in one timestamped format, so a debug run reads as
//! the operational line plus its detail.

use std::sync::OnceLock;
use std::time::Instant;

fn enabled_cell() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("MYNA_DEBUG").ok().as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("on")
        )
    })
}

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Whether debug instrumentation is enabled (`MYNA_DEBUG`).
pub fn enabled() -> bool {
    enabled_cell()
}

fn emit(stage: &str, msg: &str) {
    let t = start().elapsed().as_secs_f64();
    eprintln!("[myna +{t:7.3}s] {stage}: {msg}");
}

/// Emit one operational line for `stage` - always, `MYNA_DEBUG` or not. Only
/// for lifecycle facts, and never for anything transcript-bearing.
pub fn info(stage: &str, msg: impl AsRef<str>) {
    emit(stage, msg.as_ref());
}

/// Emit one timestamped debug line for `stage`, prefixed so pipeline stages are
/// easy to grep (`[myna +1.234s] capture: …`). No-op unless [`enabled`].
pub fn log(stage: &str, msg: impl AsRef<str>) {
    if !enabled_cell() {
        return;
    }
    emit(stage, msg.as_ref());
}

/// Convenience: `info_log!("stage", "fmt {}", val)` - the always-on tier.
#[macro_export]
macro_rules! info_log {
    ($stage:expr, $($arg:tt)*) => {
        $crate::debug::info($stage, format!($($arg)*));
    };
}

/// Convenience: `dbg_log!("stage", "fmt {}", val)` - formats only when enabled.
#[macro_export]
macro_rules! dbg_log {
    ($stage:expr, $($arg:tt)*) => {
        if $crate::debug::enabled() {
            $crate::debug::log($stage, format!($($arg)*));
        }
    };
}
