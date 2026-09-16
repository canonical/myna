//! Fire-and-forget PipeWire playback of one short, fixed mono S16LE clip (the
//! dictation start/stop/error chimes, `myna-desktop`'s `chime` module).
//! Negotiates a stereo output stream and duplicates the mono source into both
//! channels itself (see [`try_run`]'s `process` callback) rather than relying
//! on the graph's mono upmix, which routed to one channel only on at least
//! one real device.
//!
//! Mirrors [`crate::native`]'s single-purpose main-loop-thread pattern in the
//! output direction: connect, stream the clip once (padding with silence past
//! its end so the graph never sees an underrun), stop after the clip's
//! duration plus a drain margin, quit. No queue, no backpressure policy, no
//! reconnect — a chime is one buffer played once, and a failed chime must
//! never be allowed to affect dictation (every error here is swallowed, not
//! propagated).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use pipewire::{
    context::ContextRc,
    keys,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{
        param::{
            audio::{AudioFormat, AudioInfoRaw},
            ParamType,
        },
        pod::{serialize::PodSerializer, Object, Pod, Value},
        utils::{Direction, SpaTypes},
    },
    stream::{StreamFlags, StreamRc},
};

/// How much extra time to keep the stream open past the clip's nominal
/// duration, so the graph has drained the last buffer to the sink before the
/// stream disconnects.
const DRAIN_MARGIN: Duration = Duration::from_millis(150);

/// Bytes per mono S16LE sample.
const MONO_SAMPLE_BYTES: usize = 2;
/// Bytes per output (stereo S16LE) frame — the negotiated format we actually
/// stream, see [`try_run`].
const STEREO_FRAME_BYTES: usize = 4;

/// A fixed mono S16LE PCM clip.
#[derive(Clone, Copy)]
pub struct Clip {
    /// Interleaved (trivially, mono) S16LE samples.
    pub samples: &'static [u8],
    pub rate_hz: u32,
}

impl Clip {
    fn duration(&self) -> Duration {
        let frames = self.samples.len() / MONO_SAMPLE_BYTES;
        Duration::from_secs_f64(frames as f64 / self.rate_hz as f64)
    }
}

/// Play `clip` on a dedicated PipeWire thread and return immediately — the
/// caller (the desktop's activity indicator) must never block on a chime
/// finishing. Best-effort: a missing sink, a denied `pipewire` plug, or any
/// other failure is swallowed (only logged at debug level), never surfaced.
pub fn play(clip: Clip) {
    std::thread::Builder::new()
        .name("myna-pw-chime".into())
        .spawn(move || run(&clip))
        .expect("spawning the PipeWire chime thread");
}

fn run(clip: &Clip) {
    if let Err(e) = try_run(clip) {
        myna_core::dbg_log!("chime", "playback failed: {e}");
    }
}

fn try_run(clip: &Clip) -> Result<(), String> {
    let main_loop =
        MainLoopRc::new(None).map_err(|e| format!("cannot create PipeWire loop: {e}"))?;
    let context =
        ContextRc::new(&main_loop, None).map_err(|e| format!("cannot create context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("cannot connect to PipeWire: {e}"))?;

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Playback",
        *keys::MEDIA_ROLE => "Notification",
        *keys::NODE_NAME => "myna-chime",
    };
    let stream = StreamRc::new(core, "myna-chime", props)
        .map_err(|e| format!("cannot create stream: {e}"))?;

    // Position in `clip.samples`, advanced from the (single-threaded) process
    // callback only.
    let cursor = Rc::new(Cell::new(0usize));
    let samples = clip.samples;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process({
            let cursor = cursor.clone();
            move |stream, _ud| {
                while let Some(mut buffer) = stream.dequeue_buffer() {
                    let datas = buffer.datas_mut();
                    let Some(data) = datas.first_mut() else {
                        continue;
                    };
                    let Some(dst) = data.data() else { continue };
                    let maxsize = dst.len();
                    let frame_capacity = maxsize / STEREO_FRAME_BYTES;
                    let pos = cursor.get();
                    let mono_available = samples.len().saturating_sub(pos) / MONO_SAMPLE_BYTES;
                    let frames = frame_capacity.min(mono_available);
                    // Duplicate each mono sample into both channels ourselves
                    // rather than negotiating a 1-channel stream: a mono
                    // stream's automatic upmix is a sink/driver decision, and
                    // on at least one real device it played the left channel
                    // only. Handing the graph already-stereo (L == R) content
                    // needs no upmix decision at all.
                    for i in 0..frames {
                        let src = &samples
                            [pos + i * MONO_SAMPLE_BYTES..pos + (i + 1) * MONO_SAMPLE_BYTES];
                        let out = i * STEREO_FRAME_BYTES;
                        dst[out..out + MONO_SAMPLE_BYTES].copy_from_slice(src);
                        dst[out + MONO_SAMPLE_BYTES..out + STEREO_FRAME_BYTES].copy_from_slice(src);
                    }
                    let written = frames * STEREO_FRAME_BYTES;
                    // Pad the rest of this period with silence rather than
                    // shrinking the chunk — a short/empty chunk reads as an
                    // underrun to some sinks.
                    dst[written..maxsize].fill(0);
                    cursor.set(pos + frames * MONO_SAMPLE_BYTES);
                    let chunk = data.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.size_mut() = maxsize as u32;
                    *chunk.stride_mut() = STEREO_FRAME_BYTES as i32;
                }
            }
        })
        .register()
        .map_err(|e| format!("cannot register stream listener: {e}"))?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S16LE);
    audio_info.set_rate(clip.rate_hz);
    audio_info.set_channels(2);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .map_err(|e| format!("serializing audio format pod: {e:?}"))?
            .0
            .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    let flags = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
    stream
        .connect(Direction::Output, None, flags, &mut params)
        .map_err(|e| format!("cannot connect playback stream: {e}"))?;

    // Stop after the clip's duration plus a drain margin — timer-driven
    // rather than buffer-exhaustion-driven, so the graph has real wall-clock
    // time to flush the last period to the sink before we disconnect.
    let stop_at = clip.duration() + DRAIN_MARGIN;
    let timer = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        move |_| main_loop.quit()
    });
    timer
        .update_timer(Some(stop_at), None)
        .into_result()
        .map_err(|e| format!("cannot arm the stop timer: {e}"))?;

    main_loop.run();
    Ok(())
}
