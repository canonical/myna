//! Voice-activity observation for the stats tap: a port of murmure's
//! `AdaptiveVad` (audio/vad.rs), fed 20 ms frames as chunks enter the ring.
//! Pure observation, like the level meter: nothing here gates, trims or
//! delays audio. It answers three questions a UI or a session policy asks
//! after the fact - when was voice last heard, how quiet does this input get,
//! how loud is the speech on it.

use std::time::Duration;

/// Analysis frame. Short enough that a word boundary lands inside a chunk,
/// long enough for a stable RMS at 16 kHz (320 samples).
pub const FRAME: Duration = Duration::from_millis(20);

/// Voice must stay above the silence threshold this long before it counts.
/// A keystroke or a cough decays through the smoother in well under this;
/// a spoken word does not.
pub const SUSTAIN: Duration = Duration::from_millis(200);

const INITIAL_FLOOR: f32 = 0.003;
const SPEECH_MIN: f32 = 0.004;
const SPEECH_MAX: f32 = 0.08;
const SILENCE_FACTOR: f32 = 0.6;

/// Per-session voice tracker. Feed it one RMS per [`FRAME`], in order.
#[derive(Clone, Debug)]
pub struct VoiceTracker {
    /// The adaptive floor that decides speech vs silence (murmure's).
    adaptive_floor: f32,
    smoothed: Option<f32>,
    started: bool,
    active_run: Duration,
    position: Duration,
    noise_floor: Option<f32>,
    speech_level: f32,
    last_voice: Option<Duration>,
}

impl Default for VoiceTracker {
    fn default() -> Self {
        Self {
            adaptive_floor: INITIAL_FLOOR,
            smoothed: None,
            started: false,
            active_run: Duration::ZERO,
            position: Duration::ZERO,
            noise_floor: None,
            speech_level: 0.0,
            last_voice: None,
        }
    }
}

impl VoiceTracker {
    /// Observe one frame's RMS (linear full scale) covering `frame` of audio.
    pub fn observe(&mut self, rms: f32, frame: Duration) {
        self.position += frame;
        if rms < self.adaptive_floor {
            self.adaptive_floor = 0.2 * rms + 0.8 * self.adaptive_floor;
        } else {
            let floor_base = self.adaptive_floor.max(SPEECH_MIN / 5.0);
            if rms <= floor_base * 10.0 {
                self.adaptive_floor = 0.005 * rms + 0.995 * self.adaptive_floor;
            }
        }
        let smoothed = match self.smoothed {
            Some(previous) => 0.3 * rms + 0.7 * previous,
            None => rms,
        };
        self.smoothed = Some(smoothed);
        self.noise_floor = Some(self.noise_floor.map_or(smoothed, |f| f.min(smoothed)));

        let speech_threshold = (self.adaptive_floor * 5.0).clamp(SPEECH_MIN, SPEECH_MAX);
        if smoothed > speech_threshold {
            self.started = true;
        }
        let silence_threshold = (self.adaptive_floor * 3.0)
            .clamp(SPEECH_MIN * SILENCE_FACTOR, SPEECH_MAX * SILENCE_FACTOR);
        let active = self.started && smoothed >= silence_threshold;
        if active {
            self.active_run += frame;
            if self.active_run >= SUSTAIN {
                self.last_voice = Some(self.position);
                self.speech_level = self.speech_level.max(smoothed);
            }
        } else {
            self.active_run = Duration::ZERO;
        }
    }

    /// The quietest smoothed level heard so far this session, or `0.0`
    /// before the first frame. A session minimum rather than the adaptive
    /// floor: the adaptive one climbs slowly and would under-report a noisy
    /// input over a short utterance.
    pub fn noise_floor(&self) -> f32 {
        self.noise_floor.unwrap_or(0.0)
    }

    /// The loudest smoothed level heard while voice was sustained, or `0.0`
    /// if no voice has been heard.
    pub fn speech_level(&self) -> f32 {
        self.speech_level
    }

    /// Capture time at the end of the last frame of sustained voice.
    pub fn last_voice(&self) -> Option<Duration> {
        self.last_voice
    }

    /// The adaptive floor the speech and silence thresholds derive from.
    /// Diagnostic: the tests pin its dynamics, nothing else reads it.
    pub fn adaptive_floor(&self) -> f32 {
        self.adaptive_floor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tracker: &mut VoiceTracker, rms: f32, duration: Duration) {
        let frames = (duration.as_millis() / FRAME.as_millis()) as usize;
        for _ in 0..frames {
            tracker.observe(rms, FRAME);
        }
    }

    #[test]
    fn digital_silence_never_counts_as_voice() {
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.0, Duration::from_secs(5));
        assert_eq!(t.last_voice(), None);
        assert_eq!(t.speech_level(), 0.0);
        assert_eq!(t.noise_floor(), 0.0);
    }

    #[test]
    fn a_quiet_room_never_counts_as_voice() {
        // -80 dBFS hiss, a healthy headset's floor.
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_secs(5));
        assert_eq!(t.last_voice(), None);
        assert!(t.noise_floor() < 2e-4, "floor {}", t.noise_floor());
    }

    #[test]
    fn sustained_speech_marks_its_last_frame() {
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_millis(300));
        feed(&mut t, 0.01, Duration::from_secs(1)); // -40 dBFS, normal speech
        assert_eq!(t.last_voice(), Some(Duration::from_millis(1300)));
        assert!(t.speech_level() > 0.009, "speech {}", t.speech_level());
        assert!(t.noise_floor() < 2e-4, "floor {}", t.noise_floor());

        // Silence afterwards moves the mark only by the smoother's short
        // decay tail, then leaves it there.
        feed(&mut t, 1e-4, Duration::from_secs(2));
        let mark = t.last_voice().expect("voice was heard");
        assert!(
            mark > Duration::from_millis(1300) && mark <= Duration::from_millis(1400),
            "mark {mark:?}"
        );
    }

    #[test]
    fn a_click_shorter_than_the_sustain_window_is_not_voice() {
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_millis(500));
        feed(&mut t, 0.05, FRAME); // one loud frame
        feed(&mut t, 1e-4, Duration::from_secs(1));
        assert_eq!(t.last_voice(), None);
    }

    #[test]
    fn a_noisy_input_reports_its_floor_and_still_hears_speech() {
        // -34 dBFS broadband noise with -26 dBFS speech on top.
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.02, Duration::from_secs(1));
        feed(&mut t, 0.05, Duration::from_secs(1));
        assert!(t.noise_floor() >= 0.019, "floor {}", t.noise_floor());
        assert!(t.last_voice().is_some());
        assert!(t.speech_level() >= 0.045, "speech {}", t.speech_level());
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn the_adaptive_floor_falls_fast_and_climbs_slowly() {
        // Down: a fifth of the way per frame.
        let mut t = VoiceTracker::default();
        t.observe(0.001, FRAME);
        assert!(close(t.adaptive_floor(), 0.0026), "{}", t.adaptive_floor());

        // Up, while the signal is within 10x of the floor: half a percent.
        let mut t = VoiceTracker::default();
        t.observe(0.02, FRAME);
        assert!(
            close(t.adaptive_floor(), 0.003_085),
            "{}",
            t.adaptive_floor()
        );

        // Not at all for a signal further above it: that is speech, not noise.
        let mut t = VoiceTracker::default();
        t.observe(0.05, FRAME);
        assert!(
            close(t.adaptive_floor(), INITIAL_FLOOR),
            "{}",
            t.adaptive_floor()
        );
    }

    #[test]
    fn a_settled_quiet_floor_ignores_speech_when_deciding_what_is_noise() {
        // The "within 10x" window has a floor of its own (SPEECH_MIN / 5), so
        // a floor that settled far below it does not climb on -34 dBFS speech.
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_secs(3));
        let settled = t.adaptive_floor();
        assert!(settled < 2e-4, "{settled}");
        feed(&mut t, 0.02, Duration::from_secs(1));
        assert!(close(t.adaptive_floor(), settled), "{}", t.adaptive_floor());
    }

    #[test]
    fn the_initial_floor_is_conservative_about_quiet_speech() {
        // -40 dBFS straight from the start never clears floor*5; the floor
        // must first settle during a pause (as `sustained_speech_marks_its_last_frame`).
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.012, Duration::from_secs(1));
        assert_eq!(t.last_voice(), None);
    }

    #[test]
    fn exactly_the_speech_threshold_does_not_start_voice() {
        // Strictly above, so a first frame sitting on the threshold itself
        // (the smoother passes it through unchanged) is not a start.
        let mut t = VoiceTracker::default();
        let threshold = (INITIAL_FLOOR * 5.0).clamp(SPEECH_MIN, SPEECH_MAX);
        feed(&mut t, threshold, Duration::from_secs(1));
        assert_eq!(t.last_voice(), None);
    }

    #[test]
    fn a_hum_between_the_two_thresholds_never_starts_voice() {
        // Above the silence threshold, below the speech threshold: it would
        // keep voice going, but it cannot start it.
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_millis(500));
        feed(&mut t, 0.003, Duration::from_secs(1));
        assert_eq!(t.last_voice(), None);
    }

    #[test]
    fn voice_ends_when_it_drops_below_three_times_the_floor() {
        // A burst starts voice with the floor still at its initial 0.003;
        // -46 dBFS afterwards is under floor*3 = 0.009, so voice ends there.
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.05, Duration::from_millis(300));
        feed(&mut t, 0.005, Duration::from_secs(1));
        let mark = t.last_voice().expect("the burst was voice");
        assert!(mark < Duration::from_millis(500), "{mark:?}");
    }

    #[test]
    fn voice_continues_down_to_the_silence_threshold_floor() {
        // With a settled quiet floor the silence threshold is its clamp
        // minimum (SPEECH_MIN * 0.6 = 0.0024): -48 dBFS still counts.
        let mut t = VoiceTracker::default();
        feed(&mut t, 1e-4, Duration::from_millis(500));
        feed(&mut t, 0.01, Duration::from_millis(300));
        feed(&mut t, 0.004, Duration::from_secs(1));
        assert_eq!(t.last_voice(), Some(Duration::from_millis(1800)));
    }

    #[test]
    fn a_very_noisy_floor_caps_the_silence_threshold() {
        // The floor climbs to ~-34 dBFS over a long noisy run; floor*3 would
        // be 0.059 but the threshold caps at SPEECH_MAX * 0.6 = 0.048, so
        // speech at 0.055 still keeps voice going.
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.02, Duration::from_secs(20));
        assert!(t.adaptive_floor() > 0.019, "{}", t.adaptive_floor());
        feed(&mut t, 0.1, Duration::from_millis(300));
        feed(&mut t, 0.055, Duration::from_secs(1));
        assert_eq!(t.last_voice(), Some(Duration::from_millis(21_300)));
    }

    #[test]
    fn the_floor_is_the_session_minimum_not_the_first_frame() {
        let mut t = VoiceTracker::default();
        feed(&mut t, 0.01, Duration::from_millis(200));
        feed(&mut t, 1e-4, Duration::from_millis(400));
        assert!(t.noise_floor() < 5e-4, "floor {}", t.noise_floor());
    }
}
