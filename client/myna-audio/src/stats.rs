//! The stats tap (audio-adapter-api §8) — capture health for a UI: level
//! meter, activity indicator, overflow warning. Pure observation: levels and
//! counters, never samples (invariant §1.4), and it never affects what is
//! sent. Updated at capture time (as chunks enter the ring), so the meter
//! moves while the push is still gated on model readiness.

use std::time::Duration;

/// Snapshot of capture health. Levels are linear full-scale `[0, 1]` (a UI
/// converts to dBFS as it likes) and assume S16LE samples — the only encoding
/// in the format universe today (audio-adapter-api §2, pending T33).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStats {
    /// RMS level of the last captured chunk.
    pub rms: f32,
    /// Peak level of the last captured chunk.
    pub peak: f32,
    /// Highest peak seen across the whole session so far. Distinguishes "the
    /// user never spoke" (stays ~0) from a mic that produced real signal — a
    /// UI can warn when a whole utterance came back near-silent (muted input
    /// or the wrong capture node).
    pub session_peak: f32,
    /// The last captured chunk touched full scale.
    pub clipped: bool,
    /// Total audio captured this session (including anything later dropped).
    pub captured: Duration,
    /// Total audio aged out by ring overflow (drop-oldest, §6) — nonzero means
    /// the transcript will start mid-utterance and the UI should say so.
    pub dropped: Duration,
}
