//! Lightweight, env-gated debug instrumentation for the client pipeline.
//!
//! Set `MYNA_DEBUG=1` to stream timestamped diagnostics to stderr from every
//! stage of the capture → transport → inject path: how much audio is captured,
//! forwarded, and sent on the wire; which liveness/transcript events arrive;
//! and what text is committed. This is the tool for answering "where did my
//! utterance go?" live, without a debugger.
//!
//! It is **off by default** and prints nothing unless the env var is set, so it
//! never violates the no-logging-transcripts-by-default invariant. When on, it
//! *does* print transcript text and lengths (you asked for it explicitly).

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

/// Emit one timestamped debug line for `stage`, prefixed so pipeline stages are
/// easy to grep (`[myna +1.234s] capture: …`). No-op unless [`enabled`].
pub fn log(stage: &str, msg: impl AsRef<str>) {
    if !enabled_cell() {
        return;
    }
    let t = start().elapsed().as_secs_f64();
    eprintln!("[myna +{t:7.3}s] {stage}: {}", msg.as_ref());
}

/// Convenience: `dbg_log!("stage", "fmt {}", val)` — formats only when enabled.
#[macro_export]
macro_rules! dbg_log {
    ($stage:expr, $($arg:tt)*) => {
        if $crate::debug::enabled() {
            $crate::debug::log($stage, format!($($arg)*));
        }
    };
}
