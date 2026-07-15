use myna_audio_adapter::convert::ConversionPipeline;
use myna_audio_adapter::format::{AudioFormat, SampleFormat};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: preprocess_check <wav-file>");
    let mut reader = hound::WavReader::open(Path::new(&path))?;
    let spec = reader.spec();
    let source = AudioFormat {
        sample_rate: spec.sample_rate,
        sample_format: match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 16) => SampleFormat::S16LE,
            (hound::SampleFormat::Float, 32) => SampleFormat::F32LE,
            _ => {
                eprintln!("unsupported input WAV format");
                std::process::exit(1);
            }
        },
        channels: spec.channels,
    };
    let target = AudioFormat::default_target();

    let bytes: Vec<u8> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .flat_map(|s| s.unwrap().to_le_bytes())
            .collect(),
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .flat_map(|s| s.unwrap().to_le_bytes())
            .collect(),
        _ => unreachable!(),
    };

    let mut pipeline = ConversionPipeline::new(source, target)?;
    let converted = pipeline.process(&bytes)?;

    println!("input bytes: {}, output bytes: {}", bytes.len(), converted.len());

    #[cfg(feature = "vad")]
    {
        use myna_audio_adapter::frame::{AudioFrame, StreamEvent, StreamItem};
        use myna_audio_adapter::preprocess::vad::VadStage;
        use myna_audio_adapter::preprocess::PreprocessStage;
        let mut vad = VadStage::new()?;
        let mut frame = AudioFrame {
            data: converted.clone(),
            format: target.clone(),
            timestamp: std::time::Duration::ZERO,
            duration: target.duration_for_bytes(converted.len()),
            seq: 0,
        };
        let events = vad.process(&mut frame)?;
        println!("VAD events: {}", events.len());
        for ev in events {
            if let StreamEvent::VoiceActivity { speaking, at } = ev {
                println!("  speaking={speaking} at={at:?}");
            }
        }
    }

    #[cfg(feature = "denoise")]
    {
        use myna_audio_adapter::frame::AudioFrame;
        use myna_audio_adapter::preprocess::denoise::DenoiseStage;
        use myna_audio_adapter::preprocess::PreprocessStage;
        let mut denoise = DenoiseStage::new()?;
        let mut frame = AudioFrame {
            data: converted,
            format: target,
            timestamp: std::time::Duration::ZERO,
            duration: std::time::Duration::ZERO,
            seq: 0,
        };
        denoise.process(&mut frame)?;
        println!("denoise processed frame");
    }

    Ok(())
}
