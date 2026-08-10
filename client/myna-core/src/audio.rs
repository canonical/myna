//! Audio types, mirroring Python `myna.core.audio` and the Rust sketch in
//! `docs/audio-adapter-api.md` §2.
//!
//! Under the audio-push invariant the *client* owns capture and pushes PCM; the
//! STT service never touches the microphone. PCM is carried as raw binary WS
//! frames, not JSON — so [`PcmChunk`] is deliberately not `Serialize`. Only
//! [`AudioFormat`] crosses the wire as JSON (inside the session config), so only
//! it derives serde.

use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Raw PCM stream description. Default is 16 kHz mono S16LE — the common
/// denominator across the candidate ASR models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u8,
    /// Signed little-endian integer sample width. 2 = S16LE.
    pub sample_width_bytes: u8,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            channels: 1,
            sample_width_bytes: 2,
        }
    }
}

impl AudioFormat {
    pub fn bytes_per_second(&self) -> u32 {
        self.sample_rate_hz * self.channels as u32 * self.sample_width_bytes as u32
    }
}

/// A contiguous slice of raw PCM in `format`. `Bytes` is cheap to clone so a
/// chunk can fan out to a level meter without copying (audio-adapter API §2/§8).
#[derive(Clone, Debug)]
pub struct PcmChunk {
    pub data: Bytes,
    pub format: AudioFormat,
}

impl PcmChunk {
    pub fn new(data: impl Into<Bytes>, format: AudioFormat) -> Self {
        Self {
            data: data.into(),
            format,
        }
    }

    pub fn duration(&self) -> Duration {
        let bps = self.format.bytes_per_second();
        if bps == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.data.len() as f64 / bps as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_format_matches_python() {
        let fmt = AudioFormat::default();
        assert_eq!(fmt.sample_rate_hz, 16_000);
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_width_bytes, 2);
        assert_eq!(fmt.bytes_per_second(), 32_000);
    }

    #[test]
    fn audio_format_wire_shape() {
        let expected: serde_json::Value = serde_json::from_str(
            r#"{"sample_rate_hz": 16000, "channels": 1, "sample_width_bytes": 2}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(AudioFormat::default()).unwrap(),
            expected
        );
    }

    #[test]
    fn duration_from_one_second_of_audio() {
        let fmt = AudioFormat::default();
        let chunk = PcmChunk::new(vec![0u8; fmt.bytes_per_second() as usize], fmt);
        assert_eq!(chunk.duration(), Duration::from_secs(1));
    }
}
