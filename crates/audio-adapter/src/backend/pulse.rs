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
use std::thread::{self, JoinHandle};
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

/// PulseAudio capture backend using the `libpulse-simple` API.
pub struct PulseAudioBackend;

impl PulseAudioBackend {
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

impl AudioBackend for PulseAudioBackend {
    fn enumerate(&self) -> Result<Vec<InputNode>, Error> {
        Ok(vec![pulse_default_node()])
    }

    fn open(&self, config: StreamConfig, mut producer: QueueProducer) -> Result<Box<dyn BackendStream>, Error> {
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
            NodeSelector::ByName(name) => Some(name.clone()),
            NodeSelector::ById(id) if id.0.as_str() == "pulse-default" => None,
            NodeSelector::Default => None,
            _ => None,
        };
        let dev_ref = dev.as_deref();

        let simple = psimple::Simple::new(
            None,                            // default server
            "myna-audio-adapter",            // client name
            pulse::stream::Direction::Record,
            dev_ref,                         // source name or default
            "speech-to-text capture",        // stream description
            &spec,
            None,                            // default channel map
            None,                            // default buffering attributes
        )
        .map_err(|e| Error::Backend(format!("PulseAudio open failed: {e}")))?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let node_id = pulse_default_node().id;
        let chunk_duration = Duration::from_millis(10);
        let chunk_bytes = format.bytes_for_duration(chunk_duration);

        let handle: JoinHandle<()> = thread::spawn(move || {
            let mut buf = vec![0u8; chunk_bytes];
            let mut seq: u64 = 0;
            let mut timestamp = Duration::ZERO;
            while running_clone.load(Ordering::Relaxed) {
                match simple.read(&mut buf) {
                    Ok(()) => {
                        producer.push_frame(buf.clone(), timestamp, chunk_duration, seq);
                        seq += 1;
                        timestamp += chunk_duration;
                    }
                    Err(e) => {
                        eprintln!("PulseAudio read error: {e}");
                        producer.push_event(StreamEvent::DeviceLost { node: node_id });
                        break;
                    }
                }
            }
        });

        Ok(Box::new(PulseAudioStream {
            running,
            handle: Some(handle),
        }))
    }
}

struct PulseAudioStream {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BackendStream for PulseAudioStream {
    fn close(&mut self) -> Result<(), Error> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Drop for PulseAudioStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
