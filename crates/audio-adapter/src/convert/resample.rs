use crate::error::Error;
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

const CHUNK_FRAMES: usize = 1024;

/// Stateful streaming resampler.
///
/// Input arriving in arbitrary chunk sizes is carried across calls and fed to
/// rubato only in full processing chunks, so continuous audio is never
/// zero-padded mid-stream and the sinc delay line is preserved between calls.
/// Call [`flush`](Self::flush) exactly once at end-of-stream to drain the
/// remaining carried input and the filter delay line.
pub struct SampleRateConverter {
    inner: SincFixedIn<f32>,
    /// Per-channel input carried over until a full chunk is available.
    pending: Vec<Vec<f32>>,
    channels: usize,
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
        let inner = SincFixedIn::new(ratio, 2.0, params, CHUNK_FRAMES, channels as usize)
            .map_err(|e| Error::UnsupportedFormat(format!("failed to create resampler: {e}")))?;
        Ok(Self {
            inner,
            pending: vec![Vec::new(); channels as usize],
            channels: channels as usize,
        })
    }

    /// Feed one chunk of planar samples; returns whatever full chunks could be
    /// resampled (possibly empty while input accumulates).
    pub fn process(&mut self, input: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, Error> {
        if input.len() != self.channels {
            return Err(Error::UnsupportedFormat(
                "resampler input channel count mismatch".into(),
            ));
        }
        let input_len = input.first().map_or(0, Vec::len);
        if input.iter().any(|c| c.len() != input_len) {
            return Err(Error::UnsupportedFormat(
                "channel lengths differ in resampler input".into(),
            ));
        }
        for (pending, chan) in self.pending.iter_mut().zip(input) {
            pending.extend_from_slice(chan);
        }

        let mut output: Vec<Vec<f32>> = vec![Vec::new(); self.channels];
        loop {
            let needed = self.inner.input_frames_next();
            if self.pending[0].len() < needed {
                break;
            }
            let chunk: Vec<Vec<f32>> = self
                .pending
                .iter_mut()
                .map(|c| c.drain(..needed).collect())
                .collect();
            let processed = self
                .inner
                .process(&chunk, None)
                .map_err(|e| Error::UnsupportedFormat(format!("resampling failed: {e}")))?;
            for (out, chan) in output.iter_mut().zip(processed) {
                out.extend(chan);
            }
        }
        Ok(output)
    }

    /// Drain carried input and the filter delay line. End-of-stream only.
    pub fn flush(&mut self) -> Result<Vec<Vec<f32>>, Error> {
        let mut output: Vec<Vec<f32>> = vec![Vec::new(); self.channels];

        if !self.pending[0].is_empty() {
            let chunk: Vec<Vec<f32>> = self.pending.iter_mut().map(std::mem::take).collect();
            let out_frames = self.inner.output_frames_next();
            let mut out: Vec<Vec<f32>> =
                (0..self.channels).map(|_| vec![0.0f32; out_frames]).collect();
            let (_, produced) = self
                .inner
                .process_partial_into_buffer(Some(&chunk), &mut out, None)
                .map_err(|e| Error::UnsupportedFormat(format!("resampling failed: {e}")))?;
            for (o, mut chan) in output.iter_mut().zip(out) {
                chan.truncate(produced);
                o.append(&mut chan);
            }
        }

        let out_frames = self.inner.output_frames_next();
        let mut out: Vec<Vec<f32>> =
            (0..self.channels).map(|_| vec![0.0f32; out_frames]).collect();
        if let Ok((_, produced)) =
            self.inner
                .process_partial_into_buffer(None::<&[Vec<f32>]>, &mut out, None)
        {
            for (o, mut chan) in output.iter_mut().zip(out) {
                chan.truncate(produced);
                o.append(&mut chan);
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Streamed 48 kHz -> 16 kHz conversion of a sine wave must preserve the
    /// signal: correct output length, energy, and frequency, with no silence
    /// injected at chunk boundaries (the historical failure mode).
    #[test]
    fn streaming_sine_preserves_length_energy_and_frequency() {
        const SRC_RATE: usize = 48_000;
        const DST_RATE: usize = 16_000;
        const FREQ: f64 = 440.0;
        const SECONDS: f64 = 1.0;

        let total = (SRC_RATE as f64 * SECONDS) as usize;
        let sine: Vec<f32> = (0..total)
            .map(|i| {
                (2.0 * std::f64::consts::PI * FREQ * i as f64 / SRC_RATE as f64).sin() as f32
            })
            .collect();

        let mut converter = SampleRateConverter::new(SRC_RATE as u32, DST_RATE as u32, 1).unwrap();
        let mut out = Vec::new();
        // Feed in 10 ms chunks (480 frames) like a live capture would.
        for chunk in sine.chunks(480) {
            let produced = converter.process(&[chunk.to_vec()]).unwrap();
            out.extend_from_slice(&produced[0]);
        }
        let tail = converter.flush().unwrap();
        out.extend_from_slice(&tail[0]);

        // Length: 1 s at 16 kHz. The end-of-stream flush drains the sinc
        // delay line, so allow a transient tail; mid-stream silence injection
        // (the historical bug) would inflate this by thousands of frames.
        let expected = (DST_RATE as f64 * SECONDS) as usize;
        let delta = out.len() as i64 - expected as i64;
        assert!(
            (-200..500).contains(&delta),
            "unexpected output length: {} vs {expected}",
            out.len()
        );

        // Energy: RMS of a unit sine is ~0.707; chunk-boundary silence
        // injection would drag this down sharply.
        let steady = &out[400..out.len() - 400];
        let rms =
            (steady.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / steady.len() as f64).sqrt();
        assert!(
            (rms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.05,
            "RMS off: {rms}"
        );

        // Frequency via zero crossings: 440 Hz -> ~880 crossings/s.
        let crossings = steady
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count() as f64
            / (steady.len() as f64 / DST_RATE as f64);
        assert!(
            (crossings - 2.0 * FREQ).abs() < 2.0 * FREQ * 0.02,
            "frequency off: {} crossings/s",
            crossings
        );
    }
}
