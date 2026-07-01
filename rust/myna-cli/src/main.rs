//! `myna-dictate` — the orchestrator demo binary (plan T41).
//!
//! Will wire `AudioSource` (WAV mock) → orchestrator FSM → `BackendClient`
//! (real Python `myna-server`) → `TextSink` (stdout), triggered from stdin —
//! the Rust equivalent of `dev/dictate.py`. A stub until the FSM (T40) lands.

fn main() {
    println!(
        "myna-dictate: orchestrator demo (stub). Protocol version {}. \
         Wire-up lands in T41.",
        myna_core::PROTOCOL_VERSION
    );
}
