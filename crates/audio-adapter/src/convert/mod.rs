//! Audio format conversion: sample format, channel layout, and sample rate.

pub mod channels;
pub mod resample;

use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use resample::SampleRateConverter;

/// Converts audio chunks from a source format to a target format.
pub struct ConversionPipeline {
    source: AudioFormat,
    target: AudioFormat,
    resampler: Option<SampleRateConverter>,
}

impl ConversionPipeline {
    /// Create a pipeline converting from `source` to `target`.
    pub fn new(source: AudioFormat, target: AudioFormat) -> Result<Self, Error> {
        if !is_conversion_supported(&source, &target) {
            return Err(Error::UnsupportedFormat(format!(
                "cannot convert from {:?} {}Hz {}ch to {:?} {}Hz {}ch",
                source.sample_format, source.sample_rate, source.channels,
                target.sample_format, target.sample_rate, target.channels,
            )));
        }

        let resampler = if source.sample_rate != target.sample_rate {
            Some(SampleRateConverter::new(source.sample_rate, target.sample_rate, source.channels)?)
        } else {
            None
        };

        Ok(Self {
            source,
            target,
            resampler,
        })
    }

    /// Convert one chunk of interleaved audio bytes.
    pub fn process(&mut self, input: &[u8]) -> Result<Vec<u8>, Error> {
        // Step 1: decode to planar f32.
        let mut planar = channels::interleaved_to_planar_f32(input, &self.source)?;

        // Step 2: resample if needed.
        if let Some(resampler) = &mut self.resampler {
            planar = resampler.process(&planar)?;
        }

        // Step 3: adjust channel count.
        planar = channels::adjust_channels(&planar, self.target.channels);

        // Step 4: encode to target format bytes.
        channels::planar_f32_to_interleaved(&planar, &self.target)
    }

    /// Drain the resampler's carried input and delay line (end-of-stream
    /// only) and convert the tail to target-format bytes.
    pub fn flush(&mut self) -> Result<Vec<u8>, Error> {
        let Some(resampler) = &mut self.resampler else {
            return Ok(Vec::new());
        };
        let planar = resampler.flush()?;
        if planar.first().is_none_or(Vec::is_empty) {
            return Ok(Vec::new());
        }
        let planar = channels::adjust_channels(&planar, self.target.channels);
        channels::planar_f32_to_interleaved(&planar, &self.target)
    }

    /// Update the source format (e.g., on mid-stream renegotiation).
    pub fn renegotiate_source(&mut self, new_source: AudioFormat) -> Result<(), Error> {
        if !is_conversion_supported(&new_source, &self.target) {
            return Err(Error::UnsupportedFormat(format!(
                "cannot renegotiate to {:?} {}Hz {}ch",
                new_source.sample_format, new_source.sample_rate, new_source.channels,
            )));
        }
        if new_source.sample_rate != self.target.sample_rate {
            self.resampler = Some(SampleRateConverter::new(
                new_source.sample_rate,
                self.target.sample_rate,
                new_source.channels,
            )?);
        } else {
            self.resampler = None;
        }
        self.source = new_source;
        Ok(())
    }

    /// Source format currently in use.
    pub fn source(&self) -> &AudioFormat {
        &self.source
    }

    /// Target format.
    pub fn target(&self) -> &AudioFormat {
        &self.target
    }
}

fn is_conversion_supported(source: &AudioFormat, target: &AudioFormat) -> bool {
    // We support any combination of the implemented sample formats and arbitrary channel counts.
    matches!(
        (source.sample_format, target.sample_format),
        (SampleFormat::S16LE, SampleFormat::S16LE)
            | (SampleFormat::S16LE, SampleFormat::F32LE)
            | (SampleFormat::F32LE, SampleFormat::S16LE)
            | (SampleFormat::F32LE, SampleFormat::F32LE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{AudioFormat, SampleFormat};

    #[test]
    fn convert_stereo_s16_to_mono_s16() {
        let source = AudioFormat {
            sample_rate: 16_000,
            sample_format: SampleFormat::S16LE,
            channels: 2,
        };
        let target = AudioFormat::default_target();
        let samples = vec![1000i16; 16_000 * 2]; // 1 second of stereo
        let input: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut pipeline = ConversionPipeline::new(source, target.clone()).unwrap();
        let output = pipeline.process(&input).unwrap();
        let output_frames = output.len() / target.frame_size_bytes();
        assert_eq!(output_frames, 16_000);
    }

    #[test]
    fn resample_48k_to_16k_mono_s16() {
        let source = AudioFormat {
            sample_rate: 48_000,
            sample_format: SampleFormat::S16LE,
            channels: 1,
        };
        let target = AudioFormat::default_target();
        let input: Vec<u8> = vec![0i16; 48_000].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut pipeline = ConversionPipeline::new(source, target.clone()).unwrap();
        let mut output = pipeline.process(&input).unwrap();
        output.extend(pipeline.flush().unwrap());
        // Resampler may produce slightly more/less than exactly 1 s due to
        // filter transients; accept approximate.
        let output_frames = output.len() / target.frame_size_bytes();
        assert!(output_frames >= 15_800 && output_frames <= 16_500, "unexpected output frame count: {output_frames}");
    }
}
