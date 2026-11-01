//! The stats tap — capture health for a UI: level
//! meter, activity indicator, overflow warning. Pure observation: levels and
//! counters, never samples (invariant §1.4), and it never affects what is
//! sent. Updated at capture time (as chunks enter the ring), so the meter
//! moves while the push is still gated on model readiness.

use std::time::Duration;

/// Snapshot of capture health. Levels are linear full-scale `[0, 1]` (a UI
/// converts to dBFS as it likes) and assume S16LE samples — the only encoding
/// in the format universe today.
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
    /// Total audio captured this session.
    pub captured: Duration,
    /// The quietest ~60 ms heard this session, linear full scale. A UI reads
    /// it against the speech level to tell a noisy input from a quiet room.
    pub noise_floor: f32,
    /// The loudest smoothed level heard while voice was sustained; `0.0`
    /// until voice has been heard.
    pub speech_level: f32,
    /// The median of this session's ~100 ms chunk means, signed linear full
    /// scale: the input's DC offset, which a thump as the stream opens does
    /// not move. A healthy input sits within a thousandth of zero; issue
    /// #64's microphone sat at -0.22.
    pub offset: f32,
    /// The session's speech-band signal-to-noise ratio in dB, blind and
    /// unmoved by gain (see [`crate::snr::SnrEstimator`]); `None` until half
    /// a second of non-silent audio has been heard.
    pub snr_db: Option<f32>,
    /// Capture time at which sustained voice was last heard (see
    /// [`crate::voice::VoiceTracker`]); `None` until it has been. Measured in
    /// captured audio, not wall-clock, so a stalled device does not read as
    /// a silent user.
    pub last_voice: Option<Duration>,
}
