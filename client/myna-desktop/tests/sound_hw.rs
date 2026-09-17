//! Env-gated real PipeWire playback suite (`MYNA_PIPEWIRE_TESTS=1`) — feature
//! 011-accessible-dictation-ux, T055.
//!
//! Constructs the real `PipeWireSoundCuePlayer` (T057) and plays a short
//! embedded/synthesized cue clip against a live PipeWire graph, asserting no
//! error. Mirrors `tests/atspi_hw.rs`'s gate convention exactly, reusing the
//! same `MYNA_PIPEWIRE_TESTS` env var this repo's `myna-audio` capture suite
//! already established for "a real PipeWire graph is needed" (not a new
//! gate).
//!
//! ```sh
//! MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-desktop --test sound_hw
//! ```
//!
//! Skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline (constitution Principle II).

use myna_desktop::sound::{CueKind, PipeWireSoundCuePlayer};

fn pipewire_enabled() -> bool {
    std::env::var("MYNA_PIPEWIRE_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if pipewire_enabled() {
        eprintln!("MYNA_PIPEWIRE_TESTS set: the real playback-stream assertions run below");
    } else {
        eprintln!("skipping sound_hw: set MYNA_PIPEWIRE_TESTS=1 with a real PipeWire graph reachable");
    }
}

// ── T055/T057: the real backend plays each cue against a live graph ────────

#[test]
fn real_pipewire_plays_every_cue_without_error() {
    if !pipewire_enabled() {
        return;
    }
    for cue in [CueKind::SessionStart, CueKind::SessionEnd, CueKind::Failure] {
        PipeWireSoundCuePlayer::play_blocking(cue)
            .expect("a real PipeWire graph should accept a short playback stream");
    }
}
