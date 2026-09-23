//! Converts a clip's PCM to the format a backend takes. The daemon never needs
//! this, because PipeWire's graph resamples and downmixes capture, but a WAV
//! file arrives at whatever rate, channel count and width it was recorded in.

use audioadapter_buffers::direct::InterleavedSlice;
use myna_core::AudioFormat;
use rubato::{Fft, FixedSync, Resampler};

/// `pcm` in `from`, re-encoded as `to`, which must be mono S16LE. Trailing
/// bytes short of a whole frame are dropped.
pub fn convert(from: AudioFormat, pcm: &[u8], to: AudioFormat) -> Result<Vec<u8>, String> {
    assert_eq!(
        (to.channels, to.sample_width_bytes),
        (1, 2),
        "mono S16LE only"
    );
    if from == to {
        return Ok(pcm.to_vec());
    }
    let width = usize::from(from.sample_width_bytes);
    if !matches!(width, 1..=4) {
        return Err(format!("{}-bit PCM is not supported", width * 8));
    }
    let channels = usize::from(from.channels);
    let mono: Vec<f32> = pcm
        .chunks_exact(width * channels)
        .map(|frame| frame.chunks_exact(width).map(decode).sum::<f32>() / channels as f32)
        .collect();
    let mono = if from.sample_rate_hz == to.sample_rate_hz {
        mono
    } else {
        resample(&mono, from.sample_rate_hz, to.sample_rate_hz)?
    };
    Ok(mono
        .into_iter()
        .flat_map(|s| ((s * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16).to_le_bytes())
        .collect())
}

/// One little-endian sample scaled to [-1, 1). 8-bit WAV is unsigned, wider
/// widths are signed.
fn decode(bytes: &[u8]) -> f32 {
    match *bytes {
        [b] => (f32::from(b) - 128.0) / 128.0,
        [a, b] => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
        // Placed in the top three bytes, so the shift sign-extends.
        [a, b, c] => (i32::from_le_bytes([0, a, b, c]) >> 8) as f32 / 8_388_608.0,
        [a, b, c, d] => i32::from_le_bytes([a, b, c, d]) as f32 / 2_147_483_648.0,
        _ => unreachable!("width checked by the caller"),
    }
}

fn resample(input: &[f32], from_hz: u32, to_hz: u32) -> Result<Vec<f32>, String> {
    let mut resampler = Fft::<f32>::new(
        from_hz as usize,
        to_hz as usize,
        1024,
        1,
        1,
        FixedSync::Both,
    )
    .map_err(|e| format!("cannot resample {from_hz} Hz to {to_hz} Hz: {e}"))?;
    let mut output = vec![0.0; resampler.process_all_needed_output_len(input.len())];
    let input_adapter = InterleavedSlice::new(input, 1, input.len()).map_err(|e| e.to_string())?;
    let output_len = output.len();
    let mut output_adapter =
        InterleavedSlice::new_mut(&mut output, 1, output_len).map_err(|e| e.to_string())?;
    let (_, frames) = resampler
        .process_all_into_buffer(&input_adapter, &mut output_adapter, input.len(), None)
        .map_err(|e| e.to_string())?;
    output.truncate(frames);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: AudioFormat = AudioFormat {
        sample_rate_hz: 16_000,
        channels: 1,
        sample_width_bytes: 2,
    };

    fn format(rate: u32, channels: u8, width: u8) -> AudioFormat {
        AudioFormat {
            sample_rate_hz: rate,
            channels,
            sample_width_bytes: width,
        }
    }

    fn s16(samples: impl IntoIterator<Item = i16>) -> Vec<u8> {
        samples.into_iter().flat_map(i16::to_le_bytes).collect()
    }

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    fn tone(rate: u32, hz: f64, seconds: f64, channels: usize) -> Vec<u8> {
        let frames = (f64::from(rate) * seconds) as usize;
        s16((0..frames).flat_map(|i| {
            let t = i as f64 / f64::from(rate);
            let v = (16_000.0 * (2.0 * std::f64::consts::PI * hz * t).sin()) as i16;
            std::iter::repeat(v).take(channels)
        }))
    }

    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    #[test]
    fn the_target_format_passes_through_unchanged() {
        let pcm = s16([1, -2, 32_767, -32_768]);
        assert_eq!(convert(TARGET, &pcm, TARGET).unwrap(), pcm);
    }

    #[test]
    fn channels_are_averaged() {
        let pcm = s16([16_384, 0, -8_192, -8_192]);
        let out = convert(format(16_000, 2, 2), &pcm, TARGET).unwrap();
        assert_eq!(samples(&out), [8_192, -8_192]);
    }

    #[test]
    fn every_integer_width_decodes_to_the_same_level() {
        let half = [
            (1, vec![0xC0]),
            (3, vec![0x00, 0x00, 0x40]),
            (4, vec![0x00, 0x00, 0x00, 0x40]),
        ];
        for (width, pcm) in half {
            let out = convert(format(16_000, 1, width), &pcm, TARGET).unwrap();
            assert_eq!(samples(&out), [16_384], "width {width}");
        }
    }

    #[test]
    fn negative_24_bit_samples_are_sign_extended() {
        let out = convert(format(16_000, 1, 3), &[0x00, 0x00, 0xC0], TARGET).unwrap();
        assert_eq!(samples(&out), [-16_384]);
    }

    #[test]
    fn an_unsupported_width_is_refused() {
        let err = convert(format(16_000, 1, 5), &[0; 5], TARGET).unwrap_err();
        assert!(err.contains("40-bit"), "{err}");
    }

    #[test]
    fn a_partial_trailing_frame_is_dropped() {
        let out = convert(format(16_000, 2, 2), &[0, 0, 0, 0, 7], TARGET).unwrap();
        assert_eq!(samples(&out), [0]);
    }

    #[test]
    fn resampling_keeps_duration_and_pitch() {
        let out =
            samples(&convert(format(48_000, 2, 2), &tone(48_000, 440.0, 1.0, 2), TARGET).unwrap());
        assert_eq!(out.len(), 16_000);
        // Skip the filter's settling at either end.
        let body = &out[800..15_200];
        let crossings = body.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        let expected = 2.0 * 440.0 * body.len() as f64 / 16_000.0;
        assert!(
            (crossings as f64 - expected).abs() <= 2.0,
            "{crossings} vs {expected}"
        );
        assert!(
            (rms(body) - 16_000.0 / 2f64.sqrt()).abs() < 200.0,
            "{}",
            rms(body)
        );
    }

    #[test]
    fn content_above_the_new_nyquist_is_filtered_not_aliased() {
        let out = samples(
            &convert(
                format(48_000, 1, 2),
                &tone(48_000, 12_000.0, 1.0, 1),
                TARGET,
            )
            .unwrap(),
        );
        assert!(rms(&out[800..15_200]) < 50.0, "{}", rms(&out[800..15_200]));
    }
}
