use crate::backend::{AudioBackend, BackendStream};
use crate::config::StreamConfig;
use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use crate::frame::StreamEvent;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueProducer;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use std::convert::TryInto;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn sample_format_to_pw(fmt: SampleFormat) -> spa::param::audio::AudioFormat {
    match fmt {
        SampleFormat::S16LE => spa::param::audio::AudioFormat::S16le,
        SampleFormat::F32LE => spa::param::audio::AudioFormat::F32le,
    }
}

fn pw_format_to_sample(fmt: spa::param::audio::AudioFormat) -> Result<SampleFormat, Error> {
    match fmt {
        spa::param::audio::AudioFormat::S16le => Ok(SampleFormat::S16LE),
        spa::param::audio::AudioFormat::F32le => Ok(SampleFormat::F32LE),
        _ => Err(Error::UnsupportedFormat(format!(
            "PipeWire negotiated unsupported sample format: {fmt:?}"
        ))),
    }
}

fn pipewire_default_node() -> InputNode {
    InputNode {
        id: NodeId::new("pipewire-default"),
        name: "pipewire-default".into(),
        description: "Default PipeWire input".into(),
        is_default: true,
        supported_formats: vec![AudioFormat::default_target()],
    }
}

struct PipeWireUserData {
    format: spa::param::audio::AudioInfoRaw,
}

impl Default for PipeWireUserData {
    fn default() -> Self {
        Self {
            format: spa::param::audio::AudioInfoRaw::new(),
        }
    }
}

/// Native PipeWire capture backend.
pub struct PipeWireBackend {
    mainloop: pw::main_loop::MainLoop,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, Error> {
        pw::init();
        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::Backend(format!("PipeWire mainloop creation failed: {e}")))?;
        // Verify we can connect to the core; if this fails the daemon is unavailable.
        let _core = mainloop.context().connect(None).map_err(|e| {
            Error::Backend(format!("PipeWire daemon not available: {e}"))
        })?;
        Ok(Self { mainloop })
    }
}

impl AudioBackend for PipeWireBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        // TODO: enumerate nodes from the PipeWire registry once the binding's async
        // registry API is wired up. For now we expose the default node so callers can
        // open a stream immediately.
        Ok(vec![pipewire_default_node()])
    }

    fn open(&self, config: StreamConfig, mut producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error> {
        let core = self.mainloop.context().connect(None).map_err(|e| {
            Error::Backend(format!("PipeWire core connection failed: {e}"))
        })?;

        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Speech",
        };

        let target_format = config.target_format.clone();
        let node_id = pipewire_default_node().id;
        let chunk_duration = Duration::from_millis(10);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let stream = pw::stream::Stream::new(&core, "myna-audio-adapter", props)
            .map_err(|e| Error::Backend(format!("PipeWire stream creation failed: {e}")))?;

        let _listener = stream
            .add_local_listener_with_user_data(PipeWireUserData::default())
            .param_changed(|_, user_data, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let (media_type, media_subtype) =
                    match spa::param::format_utils::parse_format(param) {
                        Ok(v) => v,
                        Err(_) => return,
                    };
                if media_type != spa::param::format::MediaType::Audio
                    || media_subtype != spa::param::format::MediaSubtype::Raw
                {
                    return;
                }
                if user_data.format.parse(param).is_err() {
                    return;
                }
            })
            .process(move |stream, user_data| {
                if user_data.format.rate() == 0 {
                    return;
                }
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let n_channels = user_data.format.channels();
                let n_samples = if user_data.format.format() == spa::param::audio::AudioFormat::F32le {
                    data.chunk().size() / std::mem::size_of::<f32>() as u32
                } else {
                    data.chunk().size() / std::mem::size_of::<i16>() as u32
                };
                let frame_count = (n_samples / n_channels) as usize;
                let duration = Duration::from_secs_f64(
                    frame_count as f64 / user_data.format.rate() as f64,
                );

                if let Some(samples) = data.data() {
                    if producer.format().sample_format == SampleFormat::S16LE
                        && user_data.format.format() == spa::param::audio::AudioFormat::S16le
                    {
                        // Same format: push directly.
                        let bytes = (frame_count * n_channels as usize * 2) as usize;
                        producer.push_frame(samples[..bytes].to_vec(), Duration::ZERO, duration, 0);
                    } else {
                        // Cross-format path: decode to S16LE and push.
                        let Some(fmt) = pw_format_to_sample(user_data.format.format()).ok() else { return };
                        let src_format = AudioFormat {
                            sample_rate: user_data.format.rate(),
                            sample_format: fmt,
                            channels: n_channels as u16,
                        };
                        let converted = convert_samples_to_target(samples, &src_format, &target_format);
                        producer.push_frame(converted, Duration::ZERO, duration, 0);
                    }
                }
            })
            .register()
            .map_err(|e| Error::Backend(format!("PipeWire listener registration failed: {e}")))?;

        let mut audio_info = spa::param::audio::AudioInfoRaw::new();
        audio_info.set_format(sample_format_to_pw(config.target_format.sample_format));
        audio_info.set_rate(config.target_format.sample_rate);
        audio_info.set_channels(config.target_format.channels as u32);

        let obj = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        };
        let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(obj),
        )
        .map_err(|e| Error::Backend(format!("PipeWire format serialization failed: {e}")))?
        .0
        .into_inner();

        let mut params = [spa::pod::Pod::from_bytes(&values)
            .ok_or_else(|| Error::Backend("invalid PipeWire format pod".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::Backend(format!("PipeWire stream connect failed: {e}")))?;

        // TODO: track timestamps relative to stream start; for now frames carry chunk duration.
        let handle: JoinHandle<()> = thread::spawn(move || {
            while running_clone.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
        });

        Ok(Box::new(PipeWireStream {
            running,
            handle: Some(handle),
            stream,
            _listener: _listener,
        }))
    }
}

/// Decode `src` from `src_format` to S16LE interleaved bytes in `target_format`.
fn convert_samples_to_target(src: &[u8], src_format: &AudioFormat, target: &AudioFormat) -> Vec<u8> {
    // Convert input bytes to normalized planar f32.
    let frames = src.len() / src_format.frame_size_bytes();
    let mut planar: Vec<Vec<f32>> = (0..src_format.channels as usize)
        .map(|_| Vec::with_capacity(frames))
        .collect();

    match src_format.sample_format {
        SampleFormat::S16LE => {
            let samples: &[i16] = bytemuck::cast_slice(src);
            for frame in samples.chunks(src_format.channels as usize) {
                for (ch, s) in frame.iter().enumerate() {
                    planar[ch].push(*s as f32 / i16::MAX as f32);
                }
            }
        }
        SampleFormat::F32LE => {
            let samples: &[f32] = bytemuck::cast_slice(src);
            for frame in samples.chunks(src_format.channels as usize) {
                for (ch, s) in frame.iter().enumerate() {
                    planar[ch].push(*s);
                }
            }
        }
    }

    // Channel mixdown to target channel count.
    if target.channels == 1 && src_format.channels > 1 {
        let mut mono = vec![0.0f32; frames];
        for chan in &planar {
            for (i, s) in chan.iter().enumerate() {
                mono[i] += *s;
            }
        }
        let ch_count = src_format.channels as f32;
        for s in &mut mono {
            *s /= ch_count;
        }
        planar = vec![mono];
    }

    // Resample if needed.
    if target.sample_rate != src_format.sample_rate {
        // Reuse the crate's resampler via a tiny wrapper.
        // Import the existing converter lazily to avoid circular deps.
        planar = planar; // TODO: resample
    }

    // Convert to target format bytes.
    let mut output: Vec<u8> = Vec::with_capacity(frames * target.frame_size_bytes());
    match target.sample_format {
        SampleFormat::S16LE => {
            for i in 0..frames {
                for chan in &planar[..target.channels as usize] {
                    let sample = (chan.get(i).copied().unwrap_or(0.0).clamp(-1.0, 1.0)
                        * i16::MAX as f32)
                        as i16;
                    output.extend_from_slice(&sample.to_le_bytes());
                }
            }
        }
        SampleFormat::F32LE => {
            for i in 0..frames {
                for chan in &planar[..target.channels as usize] {
                    let sample = chan.get(i).copied().unwrap_or(0.0);
                    output.extend_from_slice(&sample.to_le_bytes());
                }
            }
        }
    }
    output
}

struct PipeWireStream {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    stream: pw::stream::Stream,
    // Keep listener alive for the stream lifetime.
    _listener: pw::stream::StreamListener<PipeWireUserData>,
}

impl BackendStream for PipeWireStream {
    fn close(&mut self) -> Result<(), Error> {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.stream.disconnect();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
