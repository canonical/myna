use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use std::cmp::Ordering;

/// Convert interleaved audio samples from `input` format to a normalized `f32`
/// planar representation (one Vec per channel).
pub fn interleaved_to_planar_f32(
    input: &[u8],
    format: &AudioFormat,
) -> Result<Vec<Vec<f32>>, Error> {
    let frame_size = format.frame_size_bytes();
    if input.len() % frame_size != 0 {
        return Err(Error::UnsupportedFormat(
            "input byte length is not a multiple of frame size".into(),
        ));
    }
    let frames = input.len() / frame_size;
    let mut channels: Vec<Vec<f32>> = (0..format.channels)
        .map(|_| Vec::with_capacity(frames))
        .collect();

    for f in 0..frames {
        let frame_offset = f * frame_size;
        for c in 0..format.channels as usize {
            let sample = &input[frame_offset + c * format.sample_format.size_bytes()
                ..frame_offset + (c + 1) * format.sample_format.size_bytes()];
            let value = decode_sample(sample, format.sample_format)?;
            channels[c].push(value);
        }
    }
    Ok(channels)
}

/// Convert normalized `f32` planar samples to interleaved bytes in `target` format.
pub fn planar_f32_to_interleaved(
    channels: &[Vec<f32>],
    target: &AudioFormat,
) -> Result<Vec<u8>, Error> {
    if channels.is_empty() {
        return Ok(Vec::new());
    }
    let frames = channels[0].len();
    if channels.iter().any(|c| c.len() != frames) {
        return Err(Error::UnsupportedFormat(
            "channel lengths differ in planar data".into(),
        ));
    }
    let sample_size = target.sample_format.size_bytes();
    let mut output = vec![0u8; frames * target.frame_size_bytes()];

    for f in 0..frames {
        let frame_offset = f * target.frame_size_bytes();
        for c in 0..target.channels as usize {
            // Guard before indexing: zero-fill channels the source lacks.
            let mixed = if c < channels.len() { channels[c][f] } else { 0.0 };
            encode_sample(
                mixed,
                target.sample_format,
                &mut output[frame_offset + c * sample_size..frame_offset + (c + 1) * sample_size],
            )?;
        }
    }
    Ok(output)
}

/// Mixdown `n` channels to 1 by averaging (for mono target from multi-channel source).
pub fn mixdown_to_mono(channels: &[Vec<f32>]) -> Vec<f32> {
    if channels.is_empty() {
        return Vec::new();
    }
    let frames = channels[0].len();
    (0..frames)
        .map(|f| {
            let sum: f32 = channels.iter().map(|c| c[f]).sum();
            sum / channels.len() as f32
        })
        .collect()
}

/// Expand mono source to all channels of target by copying.
pub fn expand_mono(channels: &[Vec<f32>], target_channels: u16) -> Vec<Vec<f32>> {
    let mono = channels.first().cloned().unwrap_or_default();
    (0..target_channels as usize)
        .map(|_| mono.clone())
        .collect()
}

/// Reorder channels to match target channel count.
pub fn adjust_channels(channels: &[Vec<f32>], target_channels: u16) -> Vec<Vec<f32>> {
    match channels.len().cmp(&(target_channels as usize)) {
        Ordering::Equal => channels.to_vec(),
        Ordering::Greater => {
            if target_channels == 1 {
                vec![mixdown_to_mono(channels)]
            } else {
                channels[..target_channels as usize].to_vec()
            }
        }
        Ordering::Less => {
            if channels.len() == 1 {
                expand_mono(channels, target_channels)
            } else {
                let mut out = channels.to_vec();
                while out.len() < target_channels as usize {
                    out.push(vec![0.0; out[0].len()]);
                }
                out
            }
        }
    }
}

fn decode_sample(bytes: &[u8], format: SampleFormat) -> Result<f32, Error> {
    match format {
        SampleFormat::S16LE => {
            if bytes.len() != 2 {
                return Err(Error::UnsupportedFormat("bad S16LE sample length".into()));
            }
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / i16::MAX as f32;
            Ok(sample.clamp(-1.0, 1.0))
        }
        SampleFormat::F32LE => {
            if bytes.len() != 4 {
                return Err(Error::UnsupportedFormat("bad F32LE sample length".into()));
            }
            Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
    }
}

fn encode_sample(value: f32, format: SampleFormat, out: &mut [u8]) -> Result<(), Error> {
    match format {
        SampleFormat::S16LE => {
            if out.len() != 2 {
                return Err(Error::UnsupportedFormat("bad S16LE output length".into()));
            }
            let clamped = value.clamp(-1.0, 1.0);
            let sample = (clamped * i16::MAX as f32) as i16;
            out.copy_from_slice(&sample.to_le_bytes());
            Ok(())
        }
        SampleFormat::F32LE => {
            if out.len() != 4 {
                return Err(Error::UnsupportedFormat("bad F32LE output length".into()));
            }
            out.copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    }
}
