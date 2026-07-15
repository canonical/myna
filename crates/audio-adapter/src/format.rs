/// Supported sample formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// 16-bit signed little-endian integer.
    S16LE,
    /// 32-bit float little-endian.
    F32LE,
}

impl SampleFormat {
    /// Size of one sample in bytes.
    pub fn size_bytes(&self) -> usize {
        match self {
            SampleFormat::S16LE => 2,
            SampleFormat::F32LE => 4,
        }
    }
}

/// Description of an audio format.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Sample format.
    pub sample_format: SampleFormat,
    /// Number of channels.
    pub channels: u16,
}

impl AudioFormat {
    /// The default STT target format: 16 kHz, mono, S16LE.
    pub fn default_target() -> Self {
        Self {
            sample_rate: 16_000,
            sample_format: SampleFormat::S16LE,
            channels: 1,
        }
    }

    /// Bytes per frame (one sample per channel).
    pub fn frame_size_bytes(&self) -> usize {
        self.sample_format.size_bytes() * self.channels as usize
    }

    /// Duration represented by a given number of bytes in this format.
    pub fn duration_for_bytes(&self, bytes: usize) -> std::time::Duration {
        let frames = bytes / self.frame_size_bytes();
        let seconds = frames as f64 / self.sample_rate as f64;
        std::time::Duration::from_secs_f64(seconds)
    }

    /// Bytes required to represent a given duration in this format.
    pub fn bytes_for_duration(&self, duration: std::time::Duration) -> usize {
        let frames = (duration.as_secs_f64() * self.sample_rate as f64).ceil() as usize;
        frames * self.frame_size_bytes()
    }
}
