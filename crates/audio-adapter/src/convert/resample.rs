use crate::error::Error;
use crate::format::AudioFormat;
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

/// Stateful resampler that converts between sample rates.
pub struct SampleRateConverter {
    inner: SincFixedIn<f32>,
    target_rate: u32,
}

impl SampleRateConverter {
    /// Create a resampler from source to target sample rate.
    pub fn new(source_rate: u32, target_rate: u32, channels: u16) -> Result<Self, Error> {
        if source_rate == target_rate {
            return Err(Error::UnsupportedFormat(
                "resampler created with identical source and target rates".into(),
            ));
        }
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = target_rate as f64 / source_rate as f64;
        let inner = SincFixedIn::new(ratio, 2.0, params, 1024, channels as usize)
            .map_err(|e| Error::UnsupportedFormat(format!("failed to create resampler: {e}")))?;
        Ok(Self { inner, target_rate })
    }

    /// Resample one chunk of planar samples.
    pub fn process(&mut self, input: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, Error> {
        let channels = input.len();
        if channels == 0 {
            return Ok(Vec::new());
        }
        let input_len = input[0].len();
        if input.iter().any(|c| c.len() != input_len) {
            return Err(Error::UnsupportedFormat(
                "channel lengths differ in resampler input".into(),
            ));
        }

        let mut output: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
        let mut pos = 0;

        while pos < input_len {
            let needed = self.inner.input_frames_next();
            let end = (pos + needed).min(input_len);
            let chunk: Vec<Vec<f32>> = input
                .iter()
                .map(|chan| chan[pos..end].to_vec())
                .collect();

            let mut out_chunk = if end - pos < needed {
                // Partial final chunk: use process_partial_into_buffer.
                let out_frames = self.inner.output_frames_next();
                let mut out: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0f32; out_frames]).collect();
                let (_, produced) = self
                    .inner
                    .process_partial_into_buffer(Some(&chunk), &mut out, None)
                    .map_err(|e| Error::UnsupportedFormat(format!("resampling failed: {e}")))?;
                out.iter_mut().for_each(|c| c.truncate(produced));
                out
            } else {
                self.inner
                    .process(&chunk, None)
                    .map_err(|e| Error::UnsupportedFormat(format!("resampling failed: {e}")))?
            };

            for (out_chan, chunk_chan) in output.iter_mut().zip(out_chunk.iter_mut()) {
                out_chan.append(chunk_chan);
            }
            pos += needed;
        }

        // Flush any remaining delayed frames.
        let out_frames = self.inner.output_frames_next();
        let mut flush_out: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0f32; out_frames]).collect();
        if let Ok((_, produced)) = self.inner.process_partial_into_buffer(None::<&[Vec<f32>]>, &mut flush_out, None) {
            flush_out.iter_mut().for_each(|c| c.truncate(produced));
            for (out_chan, flush_chan) in output.iter_mut().zip(flush_out.iter_mut()) {
                out_chan.append(flush_chan);
            }
        }

        Ok(output)
    }

    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }
}

/// Convenience: resample interleaved bytes from `source` format to `target` rate.
pub fn resample_interleaved(
    input: &[u8],
    source: &AudioFormat,
    target_rate: u32,
) -> Result<Vec<u8>, Error> {
    let planar = super::channels::interleaved_to_planar_f32(input, source)?;
    if source.sample_rate == target_rate {
        return super::channels::planar_f32_to_interleaved(
            &planar,
            &AudioFormat {
                sample_rate: target_rate,
                sample_format: source.sample_format,
                channels: source.channels,
            },
        );
    }
    let mut resampler = SampleRateConverter::new(source.sample_rate, target_rate, source.channels)?;
    let output_planar = resampler.process(&planar)?;
    super::channels::planar_f32_to_interleaved(
        &output_planar,
        &AudioFormat {
            sample_rate: target_rate,
            sample_format: source.sample_format,
            channels: source.channels,
        },
    )
}
