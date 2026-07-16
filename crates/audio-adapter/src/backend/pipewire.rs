use crate::backend::{AudioBackend, BackendStream};
use crate::config::StreamConfig;
use crate::convert::ConversionPipeline;
use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use crate::frame::StreamEvent;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueProducer;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

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

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    /// Conversion pipeline, (re)built whenever the negotiated source format
    /// differs from the target (FR-017 transparent renegotiation).
    pipeline: Option<ConversionPipeline>,
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            format: spa::param::audio::AudioInfoRaw::new(),
            pipeline: None,
        }
    }
}

/// Native PipeWire capture backend. All PipeWire objects (mainloop, context,
/// core, stream) are `!Send`, so each open stream owns them on a dedicated
/// thread that runs the mainloop; `close()` signals that loop to quit through
/// a `pw::channel`.
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> Result<Self, Error> {
        // Probe availability: if we cannot connect to a PipeWire daemon now,
        // report it so the auto-probe can fall back to PulseAudio.
        pw::init();
        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::Backend(format!("PipeWire mainloop creation failed: {e}")))?;
        let context = pw::context::Context::new(&mainloop)
            .map_err(|e| Error::Backend(format!("PipeWire context creation failed: {e}")))?;
        let _core = context
            .connect(None)
            .map_err(|e| Error::Backend(format!("PipeWire daemon not available: {e}")))?;
        Ok(Self)
    }
}

impl AudioBackend for PipeWireBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        // TODO(T018): enumerate nodes from the PipeWire registry (id, name,
        // description, formats). Until then only the default node is exposed.
        Ok(vec![pipewire_default_node()])
    }

    fn open(
        &self,
        config: StreamConfig,
        producer: QueueProducer,
    ) -> Result<Box<dyn BackendStream>, Error> {
        let target_format = config.target_format.clone();
        let node_id = pipewire_default_node().id;
        let (setup_tx, setup_rx) = mpsc::channel::<Result<(), Error>>();
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();

        let handle = thread::spawn(move || {
            capture_loop(target_format, node_id, producer, setup_tx, quit_rx);
        });

        match setup_rx.recv_timeout(SETUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Box::new(PipeWireStream {
                quit: Some(quit_tx),
                handle: Some(handle),
            })),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = quit_tx.send(());
                let _ = handle.join();
                Err(Error::Backend("PipeWire stream setup timed out".into()))
            }
        }
    }
}

/// Runs on the dedicated capture thread; owns every !Send PipeWire object and
/// runs the mainloop until `quit_rx` fires.
fn capture_loop(
    target_format: AudioFormat,
    node_id: NodeId,
    producer: QueueProducer,
    setup_tx: mpsc::Sender<Result<(), Error>>,
    quit_rx: pw::channel::Receiver<()>,
) {
    macro_rules! setup_try {
        ($expr:expr, $msg:literal) => {
            match $expr {
                Ok(v) => v,
                Err(e) => {
                    let _ = setup_tx.send(Err(Error::Backend(format!("{}: {e}", $msg))));
                    return;
                }
            }
        };
    }

    pw::init();
    let mainloop = setup_try!(
        pw::main_loop::MainLoop::new(None),
        "PipeWire mainloop creation failed"
    );
    let context = setup_try!(
        pw::context::Context::new(&mainloop),
        "PipeWire context creation failed"
    );
    let core = setup_try!(context.connect(None), "PipeWire core connection failed");

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Speech",
    };

    let stream = setup_try!(
        pw::stream::Stream::new(&core, "myna-audio-adapter", props),
        "PipeWire stream creation failed"
    );

    let events_producer = producer.clone();
    let lost_node = node_id.clone();
    let process_target = target_format.clone();

    let _listener = setup_try!(
        stream
            .add_local_listener_with_user_data(UserData::default())
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
                if user_data.format.parse(param).is_ok() {
                    // Source format (re)negotiated: rebuild conversion lazily.
                    user_data.pipeline = None;
                }
            })
            .state_changed(move |_, _, _old, new| {
                if matches!(new, pw::stream::StreamState::Error(_)) {
                    events_producer.push_event(StreamEvent::DeviceLost {
                        node: lost_node.clone(),
                    });
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
                let size = data.chunk().size() as usize;
                let Some(samples) = data.data() else { return };
                let samples = &samples[..size.min(samples.len())];

                let Ok(src_sample_format) = pw_format_to_sample(user_data.format.format())
                else {
                    return;
                };
                let src_format = AudioFormat {
                    sample_rate: user_data.format.rate(),
                    sample_format: src_sample_format,
                    channels: user_data.format.channels() as u16,
                };

                if src_format == process_target {
                    // Server delivered the target format: push as-is.
                    producer.push_frame(samples.to_vec());
                    return;
                }

                // Cross-format path: (re)build the shared conversion pipeline
                // when the source format changed, then convert.
                let rebuild = match &user_data.pipeline {
                    Some(p) => *p.source() != src_format,
                    None => true,
                };
                if rebuild {
                    match ConversionPipeline::new(src_format.clone(), process_target.clone()) {
                        Ok(p) => user_data.pipeline = Some(p),
                        Err(_) => return, // unconvertible; renegotiation error path
                    }
                }
                if let Some(pipeline) = &mut user_data.pipeline {
                    if let Ok(converted) = pipeline.process(samples) {
                        if !converted.is_empty() {
                            producer.push_frame(converted);
                        }
                    }
                }
            })
            .register(),
        "PipeWire listener registration failed"
    );

    // Ask the server for the target format; PipeWire converts server-side
    // when it can (FR-009), otherwise param_changed reports the real source
    // format and the conversion pipeline covers the difference.
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(sample_format_to_pw(target_format.sample_format));
    audio_info.set_rate(target_format.sample_rate);
    audio_info.set_channels(target_format.channels as u32);

    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = setup_try!(
        spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(obj),
        )
        .map(|(c, _)| c.into_inner()),
        "PipeWire format serialization failed"
    );
    let Some(pod) = spa::pod::Pod::from_bytes(&values) else {
        let _ = setup_tx.send(Err(Error::Backend("invalid PipeWire format pod".into())));
        return;
    };
    let mut params = [pod];

    setup_try!(
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        ),
        "PipeWire stream connect failed"
    );

    // Quit signal from close(): stop the mainloop.
    let loop_clone = mainloop.clone();
    let _quit_receiver = quit_rx.attach(mainloop.loop_(), move |_| {
        loop_clone.quit();
    });

    let _ = setup_tx.send(Ok(()));

    // Dispatch callbacks until close() signals quit (or the loop errors out).
    mainloop.run();

    let _ = stream.disconnect();
}

struct PipeWireStream {
    quit: Option<pw::channel::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl BackendStream for PipeWireStream {
    fn close(&mut self) -> Result<(), Error> {
        if let Some(quit) = self.quit.take() {
            let _ = quit.send(());
        }
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
