use crate::backend::{AudioBackend, BackendStream};
use crate::config::{NodeSelector, StreamConfig};
use crate::error::Error;
use crate::format::{AudioFormat, SampleFormat};
use crate::frame::StreamEvent;
use crate::node::{InputNode, NodeId};
use crate::ring::QueueProducer;
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn sample_format_to_pulse(fmt: SampleFormat) -> Result<pulse::sample::Format, Error> {
    match fmt {
        SampleFormat::S16LE => Ok(pulse::sample::Format::S16le),
        SampleFormat::F32LE => Ok(pulse::sample::Format::F32le),
    }
}

fn pulse_default_node() -> InputNode {
    InputNode {
        id: NodeId::new("pulse-default"),
        name: "pulse-default".into(),
        description: "Default PulseAudio source".into(),
        is_default: true,
        supported_formats: vec![AudioFormat::default_target()],
    }
}

fn map_open_error(e: pulse::error::PAErr) -> Error {
    match pulse::error::Code::try_from(e) {
        Ok(pulse::error::Code::Access) => Error::PermissionDenied,
        Ok(pulse::error::Code::NoEntity) => Error::NoDevice,
        _ => Error::Backend(format!("PulseAudio open failed: {e}")),
    }
}

/// PulseAudio capture backend using the `libpulse-simple` API.
pub struct PulseAudioBackend;

impl PulseAudioBackend {
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

impl AudioBackend for PulseAudioBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        // TODO(T019): enumerate sources via the full async API; the simple API
        // has no introspection. Until then only the default source is exposed.
        Ok(vec![pulse_default_node()])
    }

    fn open(
        &self,
        config: StreamConfig,
        producer: QueueProducer,
    ) -> Result<Box<dyn BackendStream>, Error> {
        let format = config.target_format;
        let spec = pulse::sample::Spec {
            format: sample_format_to_pulse(format.sample_format)?,
            channels: format.channels as u8,
            rate: format.sample_rate,
        };
        if !spec.is_valid() {
            return Err(Error::UnsupportedFormat(
                "PulseAudio sample spec invalid".into(),
            ));
        }

        let dev: Option<String> = match &config.node {
            NodeSelector::ByName(name) if name != "pulse-default" => Some(name.clone()),
            _ => None,
        };
        let dev_ref = dev.as_deref();

        let simple = psimple::Simple::new(
            None,                     // default server
            "myna-audio-adapter",     // client name
            pulse::stream::Direction::Record,
            dev_ref,                  // source name or default
            "speech-to-text capture", // stream description
            &spec,
            None, // default channel map
            None, // default buffering attributes
        )
        .map_err(map_open_error)?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let node_id = pulse_default_node().id;
        let chunk_duration = Duration::from_millis(10);
        let chunk_bytes = format.bytes_for_duration(chunk_duration);

        thread::spawn(move || {
            let mut buf = vec![0u8; chunk_bytes];
            while running_clone.load(Ordering::Relaxed) {
                match simple.read(&mut buf) {
                    Ok(()) => {
                        // Hand off the filled buffer; the queue assigns
                        // timestamps and sequence numbers.
                        let data = std::mem::replace(&mut buf, vec![0u8; chunk_bytes]);
                        producer.push_frame(data);
                    }
                    Err(_) => {
                        producer.push_event(StreamEvent::DeviceLost {
                            node: node_id.clone(),
                        });
                        break;
                    }
                }
            }
        });

        Ok(Box::new(PulseAudioStream { running }))
    }
}

struct PulseAudioStream {
    running: Arc<AtomicBool>,
}

impl BackendStream for PulseAudioStream {
    fn close(&mut self) -> Result<(), Error> {
        // Signal the capture thread and detach. `Simple::read` blocks with no
        // timeout, so joining here could hang close() past its 200 ms budget
        // (G8/SC-004) when the source is suspended; the thread exits (and
        // drops the Pulse connection) as soon as the pending read returns.
        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for PulseAudioStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
