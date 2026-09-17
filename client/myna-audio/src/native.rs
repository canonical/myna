//! [`PipeWireBackend`] (plan T52) — native live capture via `pipewire-rs`,
//! behind the [`CaptureBackend`] seam. No subprocess: a dedicated PipeWire
//! main-loop thread owns a capture `Stream`, and its `process` callback pushes
//! PCM into the adapter's ring via [`Producer::push`]. The stream is not
//! `RT_PROCESS`: PipeWire dispatches `process` on that same loop thread, so
//! callbacks, timers and teardown are serialized and share state through
//! `Rc`/`RefCell` without locking on a realtime thread.
//!
//! Replaces the `pw-record` subprocess backend (feature
//! 002-native-pipewire-backend, FR-016). Adds what the subprocess couldn't do
//! in-process: node selection by stable `node.name` (T021), channel
//! pick/downmix on multi-channel interfaces (T025), and graph-side
//! resample/downmix to the negotiated format. Real
//! DSP stays in the PipeWire graph upstream of our node (§10) — this backend
//! only selects, converts, observes.
//!
//! The module is named `native` (not `pipewire`) to avoid colliding with the
//! `pipewire` crate.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use myna_core::CaptureError;
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
    stream::{StreamFlags, StreamRc, StreamState},
};

use crate::backend::{CaptureBackend, CaptureSpec, Producer};

/// How often the loop wakes to check the [`StopHandle`] and the phase
/// deadline, so a graceful stop / abort is honored within the ~250 ms
/// promptness contract (FR-012) in every phase, even when no audio flows.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Deadline for each phase: the daemon answering discovery, the first audio
/// after connecting, and the next audio while capturing. A daemon without a
/// session manager accepts `stream.connect` and even negotiates the stream to
/// `Paused` while nothing links it (2026-07-21 hardware finding), so only
/// delivered audio proves the capture works. Silent PCM is delivery; this is
/// never a voice-activity timeout. Healthy graphs deliver every quantum
/// (tens of milliseconds); 3 s is headroom.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(3);

/// Where capture is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting for the daemon to answer the registry roundtrip.
    Discovering,
    /// Stream connected, no audio delivered yet.
    Linking,
    /// Audio has been delivered.
    Capturing,
}

/// Why the capture loop quit.
#[derive(Debug, PartialEq)]
enum Ending {
    Clean,
    Fault(CaptureError),
}

impl Ending {
    fn into_fault(self) -> Option<CaptureError> {
        match self {
            Ending::Clean => None,
            Ending::Fault(err) => Some(err),
        }
    }
}

/// The lifecycle decisions, kept free of PipeWire and the wall clock so they
/// can be driven from tests. The loop's poll timer asks [`Supervisor::tick`]
/// whether to quit.
struct Supervisor {
    phase: Phase,
    /// The graph accepted the stream (`Paused`/`Streaming`); a device may
    /// still be resuming, so this is not proof of flow.
    wired: bool,
    deadline: Option<Instant>,
    target: Option<String>,
}

impl Supervisor {
    fn new(now: Instant, target: Option<String>) -> Self {
        Self {
            phase: Phase::Discovering,
            wired: false,
            deadline: Some(now + SILENCE_TIMEOUT),
            target,
        }
    }

    fn linking(&mut self, now: Instant) {
        self.phase = Phase::Linking;
        self.deadline = Some(now + SILENCE_TIMEOUT);
    }

    fn wired(&mut self) {
        self.wired = true;
    }

    /// A non-empty buffer arrived.
    fn delivered(&mut self, now: Instant) {
        self.phase = Phase::Capturing;
        self.deadline = Some(now + SILENCE_TIMEOUT);
    }

    fn tick(&self, now: Instant, stopped: bool) -> Option<Ending> {
        let target = self.target.as_deref();
        if stopped {
            // A stop before the graph accepted the stream is a failed
            // open, never an empty stream masquerading as a clean end (§3).
            return Some(if self.wired || self.phase == Phase::Capturing {
                Ending::Clean
            } else {
                Ending::Fault(CaptureError::DeviceUnavailable(
                    "capture stopped before an audio source was wired".into(),
                ))
            });
        }
        match self.deadline {
            Some(deadline) if now >= deadline => Some(Ending::Fault(
                CaptureError::DeviceUnavailable(match self.phase {
                    Phase::Discovering => no_answer_message(),
                    _ => no_flow_message(target),
                }),
            )),
            _ => None,
        }
    }
}

/// Lost audio tolerated before capture faults: under one default quantum
/// (1024 frames at 48 kHz), far above the sub-frame resampler jitter.
const LOSS_TOLERANCE: Duration = Duration::from_millis(20);

/// Audio the graph produced for this stream but never handed over. The loop
/// thread is not realtime, and PipeWire overwrites the buffer of each graph
/// cycle the thread sleeps through without reporting it. The stream clock
/// still advances for every cycle, so clock minus delivered frames is the
/// loss.
struct Continuity {
    rate: f64,
    last_ticks: Option<u64>,
    /// Frames owed, never negative: a surplus must not hide a later loss.
    owed: f64,
}

impl Continuity {
    fn new(rate: u32) -> Self {
        Self {
            rate: rate as f64,
            last_ticks: None,
            owed: 0.0,
        }
    }

    /// The stream changed state; nothing is owed across a pause.
    fn restart(&mut self) {
        self.last_ticks = None;
        self.owed = 0.0;
    }

    /// `frames` arrived at stream clock `ticks` (in `graph_rate` units).
    /// Returns the loss once it exceeds [`LOSS_TOLERANCE`].
    fn delivered(&mut self, ticks: u64, graph_rate: (u32, u32), frames: u64) -> Option<Duration> {
        let last = self.last_ticks.replace(ticks);
        let (num, denom) = graph_rate;
        match last {
            // A clock switch rebases ticks, so that cycle carries none.
            Some(last) if ticks > last && denom != 0 => {
                let expected = (ticks - last) as f64 * num as f64 * self.rate / denom as f64;
                self.owed = (self.owed + expected - frames as f64).max(0.0);
            }
            _ => {}
        }
        let lost = Duration::from_secs_f64(self.owed / self.rate);
        (lost > LOSS_TOLERANCE).then_some(lost)
    }
}

/// The fault when captured audio was lost to a starved capture loop.
fn lost_audio_message(lost: Duration) -> String {
    format!(
        "{} ms of audio was lost: the capture thread could not keep up, the system may be overloaded",
        lost.as_millis()
    )
}

/// The fault when the daemon never answers discovery.
fn no_answer_message() -> String {
    format!(
        "PipeWire did not answer within {} s - is the audio service running?",
        SILENCE_TIMEOUT.as_secs()
    )
}

/// The user-facing, content-free fault when the graph has no usable source
/// at open — names the usual cause (a masked/absent session manager).
fn no_source_message(target: Option<&str>) -> String {
    match target {
        Some(t) => format!(
            "no audio source available for '{t}' — is a PipeWire session manager (e.g. WirePlumber) running?"
        ),
        None => {
            "no audio source available — is a PipeWire session manager (e.g. WirePlumber) running?"
                .to_string()
        }
    }
}

/// The user-facing, content-free fault when an opened stream produced zero
/// buffers within the watchdog window — a dead stream, never a silent
/// "successful" empty capture.
fn no_flow_message(target: Option<&str>) -> String {
    match target {
        Some(t) => format!(
            "no audio is flowing from '{t}' — the device may be stuck, unplugged, or the graph failed to link"
        ),
        None => {
            "no audio is flowing from the capture source — the device may be stuck, unplugged, or the graph failed to link"
                .to_string()
        }
    }
}

/// Roundtrip the registry once and report whether the graph has a usable
/// capture source: any real `Audio/Source` (the device-enumeration mapping),
/// or — with an explicit target — a node with that `node.name`. Runs the loop
/// for a single core sync (milliseconds); globals from the initial roundtrip
/// are the complete current graph.
fn graph_has_source(
    main_loop: &MainLoopRc,
    core: &pipewire::core::CoreRc,
    target: Option<&str>,
) -> Result<bool, CaptureError> {
    let registry = core.get_registry_rc().map_err(|e| {
        CaptureError::DeviceUnavailable(format!("cannot get PipeWire registry: {e}"))
    })?;
    let found = Rc::new(Cell::new(false));
    let _listener = registry
        .add_listener_local()
        .global({
            let found = found.clone();
            let target = target.map(str::to_string);
            move |global| {
                use pipewire::types::ObjectType;
                if global.type_ != ObjectType::Node {
                    return;
                }
                let Some(props) = &global.props else { return };
                let hit = match &target {
                    Some(t) => props.get("node.name") == Some(t.as_str()),
                    None => crate::devices::map_input_device(|k| props.get(k)).is_some(),
                };
                if hit {
                    found.set(true);
                }
            }
        })
        .register();

    let seq = core
        .sync(0)
        .map_err(|e| CaptureError::Backend(format!("cannot sync PipeWire core: {e}")))?;
    let _core_listener = core
        .add_listener_local()
        .done({
            let main_loop = main_loop.clone();
            move |_id, done_seq| {
                if done_seq == seq {
                    main_loop.quit();
                }
            }
        })
        .error({
            let main_loop = main_loop.clone();
            move |_id, _seq, _res, _msg| main_loop.quit()
        })
        .register();
    main_loop.run();
    Ok(found.get())
}

/// Native PipeWire capture backend. Construct with [`PipeWireBackend::new`];
/// use through `CaptureSource::builder(fmt).backend(Box::new(...))`.
#[derive(Default)]
pub struct PipeWireBackend {
    /// Daemon remote to connect to (`remote.name`, may be an absolute socket
    /// path). `None` = the session default. Advanced/testing seam — e.g. the
    /// gated suite's private no-session-manager daemon.
    remote: Option<String>,
}

impl PipeWireBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect to a specific daemon remote instead of the session default.
    pub fn with_remote(remote: impl Into<String>) -> Self {
        Self {
            remote: Some(remote.into()),
        }
    }
}

impl CaptureBackend for PipeWireBackend {
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer) {
        // Only S16LE lives in the format universe today. Reject other widths
        // up front — cheap, testable offline
        // (T008), no PipeWire connection needed.
        if spec.format.sample_width_bytes != 2 {
            producer.finish(Some(CaptureError::UnsupportedFormat(spec.format)));
            return;
        }
        // Validate channel-index selection up front (T026): indices must be
        // non-empty and downmix to the negotiated channel count. The actual
        // pick/downmix happens graph-side + in the process callback (T025).
        if spec
            .channels
            .as_ref()
            .is_some_and(|indices| indices.is_empty())
        {
            producer.finish(Some(CaptureError::Backend(
                "channel selection is empty; give at least one channel index".into(),
            )));
            return;
        }
        // The loop and its objects are not `Send`, so everything PipeWire
        // lives on this thread. A failed spawn drops the producer, which
        // faults the capture rather than hanging it.
        let remote = self.remote;
        let _ = std::thread::Builder::new()
            .name("myna-pw-capture".into())
            .spawn(move || run_capture(spec, producer, remote));
    }
}

/// The capture thread body. Every PipeWire object lives inside
/// [`capture_session`] and is released when it returns; only then does the
/// producer deliver the one terminal outcome, so the producer's release (the
/// end of the health stream) means PipeWire is released too.
fn run_capture(spec: CaptureSpec, producer: Producer, remote: Option<String>) {
    let producer = Rc::new(RefCell::new(Some(producer)));
    let fault = capture_session(&spec, &producer, remote.as_deref());
    let producer = producer.borrow_mut().take();
    if let Some(p) = producer {
        p.finish(fault);
    }
}

/// Build loop + stream, connect, then run until stop/abort/fault. Returns the
/// terminal fault, `None` for a clean end.
fn capture_session(
    spec: &CaptureSpec,
    producer: &Rc<RefCell<Option<Producer>>>,
    remote: Option<&str>,
) -> Option<CaptureError> {
    let main_loop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            return Some(CaptureError::DeviceUnavailable(format!(
                "cannot create PipeWire loop: {e}"
            )));
        }
    };

    // Shared with the loop callbacks. Sound because every callback runs on
    // this thread (no RT_PROCESS, asserted in process). The first ending wins.
    let supervisor = Rc::new(RefCell::new(Supervisor::new(
        Instant::now(),
        spec.target.clone(),
    )));
    let ending: Rc<RefCell<Option<Ending>>> = Rc::new(RefCell::new(None));
    let end = {
        let main_loop = main_loop.clone();
        let ending = ending.clone();
        move |why: Ending| {
            ending.borrow_mut().get_or_insert(why);
            main_loop.quit();
        }
    };

    // Poll timer: stop, abort and phase deadlines are observed in every
    // phase, from discovery on, within STOP_POLL (FR-012, SC-009).
    let timer = main_loop.loop_().add_timer({
        let supervisor = supervisor.clone();
        let stop = spec.stop.clone();
        let end = end.clone();
        move |_| {
            if let Some(why) = supervisor.borrow().tick(Instant::now(), stop.is_stopped()) {
                end(why);
            }
        }
    });
    let _ = timer
        .update_timer(Some(STOP_POLL), Some(STOP_POLL))
        .into_result();

    let context = match ContextRc::new(&main_loop, None) {
        Ok(c) => c,
        Err(e) => {
            return Some(CaptureError::DeviceUnavailable(format!(
                "cannot create PipeWire context: {e}"
            )));
        }
    };
    let core = match remote {
        Some(name) => {
            let props = properties! { *keys::REMOTE_NAME => name }.to_owned();
            context.connect_rc(Some(props))
        }
        None => context.connect_rc(None),
    };
    let core = match core {
        Ok(c) => c,
        Err(e) => {
            return Some(CaptureError::DeviceUnavailable(format!(
                "cannot connect to PipeWire: {e}"
            )));
        }
    };

    // No-source graph check: refuse to open a stream the graph could never
    // feed. Skipped roundtrips would be a silent empty capture (§3).
    let found = graph_has_source(&main_loop, &core, spec.target.as_deref());
    if let Some(why) = ending.borrow_mut().take() {
        return why.into_fault();
    }
    match found {
        Ok(true) => {}
        Ok(false) => {
            return Some(CaptureError::DeviceUnavailable(no_source_message(
                spec.target.as_deref(),
            )));
        }
        Err(e) => return Some(e),
    }

    // Stream properties: an audio capture stream, optionally targeting a
    // specific node by stable node.name (T021, PW_KEY_TARGET_OBJECT).
    let mut props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Communication",
        *keys::NODE_NAME => "myna-dictate",
    };
    if let Some(target) = &spec.target {
        props.insert(*keys::TARGET_OBJECT, target.as_str());
    }

    let stream = match StreamRc::new(core, "myna-capture", props) {
        Ok(s) => s,
        Err(e) => {
            return Some(CaptureError::Backend(format!(
                "cannot create capture stream: {e}"
            )));
        }
    };

    // Channel selection (§9): when specific indices are requested, ask the
    // graph for enough channels to contain them (max index + 1), then the
    // process callback picks those indices and downmixes to the negotiated
    // channel count (T025). Otherwise request the negotiated channels directly.
    let selection = spec.channels.clone();
    let stream_channels = match &selection {
        Some(idx) => idx.iter().copied().max().map(|m| m as u32 + 1).unwrap_or(1),
        None => spec.format.channels as u32,
    };

    let continuity = Rc::new(RefCell::new(Continuity::new(spec.format.sample_rate_hz)));
    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let end = end.clone();
            let target = spec.target.clone();
            let supervisor = supervisor.clone();
            let continuity = continuity.clone();
            move |_stream, _ud, _old, new| {
                continuity.borrow_mut().restart();
                if matches!(new, StreamState::Paused | StreamState::Streaming) {
                    supervisor.borrow_mut().wired();
                }
                if let StreamState::Error(msg) = &new {
                    // A stream error mid-capture (e.g. the device/daemon went
                    // away) → one terminal fault, then quit (FR-010, C10).
                    let detail = match &target {
                        Some(t) => format!("PipeWire stream error on '{t}': {msg}"),
                        None => format!("PipeWire stream error: {msg}"),
                    };
                    end(Ending::Fault(CaptureError::DeviceUnavailable(detail)));
                }
            }
        })
        .process({
            let end = end.clone();
            let producer = producer.clone();
            let supervisor = supervisor.clone();
            // Channel pick/downmix config (§9): pick these input-channel indices
            // from the `stream_channels`-wide stream and average them down to
            // `out_channels`. `None` = pass through unchanged.
            let selection = selection.clone();
            let in_channels = stream_channels as usize;
            let out_channels = spec.format.channels as usize;
            let loop_thread = std::thread::current().id();
            move |stream, _ud| {
                debug_assert_eq!(
                    std::thread::current().id(),
                    loop_thread,
                    "process callback must run on the capture loop thread"
                );
                let mut frames = 0u64;
                while let Some(mut buffer) = stream.dequeue_buffer() {
                    let datas = buffer.datas_mut();
                    let Some(data) = datas.first_mut() else {
                        continue;
                    };
                    let size = data.chunk().size() as usize;
                    let offset = data.chunk().offset() as usize;
                    let Some(samples) = data.data() else {
                        continue;
                    };
                    let end_at = (offset + size).min(samples.len());
                    let slice = &samples[offset.min(samples.len())..end_at];
                    // Empty buffers are not delivery; silent PCM is.
                    if slice.is_empty() {
                        continue;
                    }
                    supervisor.borrow_mut().delivered(Instant::now());
                    frames += (slice.len() / (in_channels * 2)) as u64;
                    let bytes = match &selection {
                        Some(idx) => select_channels_s16(slice, in_channels, idx, out_channels),
                        None => Bytes::copy_from_slice(slice),
                    };
                    let alive = producer
                        .borrow_mut()
                        .as_mut()
                        .is_some_and(|p| p.push(bytes));
                    if !alive {
                        // Consumer gone (abort) → end promptly (FR-011).
                        end(Ending::Clean);
                    }
                }
                if frames == 0 {
                    return;
                }
                if let Ok(time) = stream.time() {
                    let rate = time.rate();
                    let lost = continuity.borrow_mut().delivered(
                        time.ticks(),
                        (rate.num, rate.denom),
                        frames,
                    );
                    if let Some(lost) = lost {
                        end(Ending::Fault(CaptureError::Backend(lost_audio_message(
                            lost,
                        ))));
                    }
                }
            }
        })
        .register();
    let listener = match listener {
        Ok(l) => l,
        Err(e) => {
            return Some(CaptureError::Backend(format!(
                "cannot register stream listener: {e}"
            )));
        }
    };

    // Request EXACTLY the negotiated format: S16LE at the negotiated
    // rate/channels. PipeWire's graph inserts the resampler/downmixer so the
    // stream delivers this regardless of the device's native format (FR-003,
    // C2), honoring "the backend owns conversion" (§7).
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S16LE);
    audio_info.set_rate(spec.format.sample_rate_hz);
    audio_info.set_channels(stream_channels);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .expect("serializing audio format pod")
            .0
            .into_inner();
    let mut params = [Pod::from_bytes(&values).expect("valid format pod")];

    // With an explicit target, forbid the reconnect/fallback that AUTOCONNECT
    // otherwise does: an unresolvable `node.name` must fault (FR-004, C4), not
    // silently capture the default device. DONT_RECONNECT drives the stream to
    // the error state, which `state_changed` turns into a terminal fault.
    let mut flags = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS;
    if spec.target.is_some() {
        flags |= StreamFlags::DONT_RECONNECT;
    }

    supervisor.borrow_mut().linking(Instant::now());
    if let Err(e) = stream.connect(Direction::Input, None, flags, &mut params) {
        return Some(match &spec.target {
            Some(t) => CaptureError::DeviceUnavailable(format!("cannot connect to '{t}': {e}")),
            None => CaptureError::Backend(format!("cannot connect capture stream: {e}")),
        });
    }

    main_loop.run();

    // Quiesce the stream before the caller takes the producer; the PipeWire
    // objects drop on return.
    let _ = stream.disconnect();
    drop(listener);
    drop(timer);
    let why = ending.borrow_mut().take();
    why.and_then(Ending::into_fault)
}

/// Pick channel indices `selected` from an interleaved S16LE frame stream that
/// has `in_channels` channels, and downmix them to `out_channels` by averaging
/// Frame-aligned; a trailing partial frame is dropped.
///
/// For `out_channels == 1`, all selected channels average into the single out
/// channel. For `out_channels == selected.len()`, each selected channel maps
/// 1:1 in order. Other cases distribute selected channels round-robin across
/// the out channels (best-effort; the common cases are mono-out and identity).
fn select_channels_s16(
    data: &[u8],
    in_channels: usize,
    selected: &[u8],
    out_channels: usize,
) -> Bytes {
    if in_channels == 0 || out_channels == 0 || selected.is_empty() {
        return Bytes::copy_from_slice(data);
    }
    let in_stride = in_channels * 2; // S16 = 2 bytes
    let frames = data.len() / in_stride;
    let mut out = Vec::with_capacity(frames * out_channels * 2);
    // Which selected indices feed each output channel.
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); out_channels];
    for (n, &ch) in selected.iter().enumerate() {
        buckets[n % out_channels].push(ch as usize);
    }
    for f in 0..frames {
        let base = f * in_stride;
        for bucket in &buckets {
            let mut acc: i32 = 0;
            let mut count: i32 = 0;
            for &ch in bucket {
                if ch < in_channels {
                    let p = base + ch * 2;
                    let s = i16::from_le_bytes([data[p], data[p + 1]]) as i32;
                    acc += s;
                    count += 1;
                }
            }
            let v = if count > 0 { (acc / count) as i16 } else { 0 };
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    Bytes::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureSource;
    use futures_util::StreamExt;
    use myna_core::{AudioFormat as CoreFormat, AudioSource, CaptureError, PcmChunk};
    use std::time::Duration;

    async fn drain(mut stream: myna_core::CaptureStream) -> (Vec<PcmChunk>, Option<CaptureError>) {
        let mut chunks = Vec::new();
        let mut fault = None;
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream stalled")
        {
            match item {
                Ok(c) => chunks.push(c),
                Err(e) => {
                    assert!(fault.is_none(), "more than one Err on the stream");
                    fault = Some(e);
                }
            }
        }
        (chunks, fault)
    }

    /// T008 (hermetic, no audio server): a non-S16 width is rejected up front
    /// as the stream's single `Err`, before any PipeWire connection (C12).
    #[tokio::test]
    async fn non_s16_width_is_unsupported() {
        let odd = CoreFormat {
            sample_rate_hz: 16_000,
            channels: 1,
            sample_width_bytes: 4,
        };
        let source = CaptureSource::builder(odd)
            .backend(Box::new(PipeWireBackend::new()))
            .build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        assert!(chunks.is_empty());
        assert!(matches!(fault, Some(CaptureError::UnsupportedFormat(_))));
    }

    const MS: Duration = Duration::from_millis(1);

    fn stopped_before_wired(ending: Option<Ending>) -> bool {
        matches!(
            ending,
            Some(Ending::Fault(CaptureError::DeviceUnavailable(msg))) if msg.contains("stopped before")
        )
    }

    fn fault_message(ending: Option<Ending>) -> String {
        match ending {
            Some(Ending::Fault(CaptureError::DeviceUnavailable(msg))) => msg,
            other => panic!("expected a DeviceUnavailable fault, got {other:?}"),
        }
    }

    #[test]
    fn discovery_faults_at_its_deadline_naming_the_daemon() {
        let t0 = Instant::now();
        let sup = Supervisor::new(t0, None);
        assert_eq!(sup.tick(t0 + SILENCE_TIMEOUT - MS, false), None);
        let msg = fault_message(sup.tick(t0 + SILENCE_TIMEOUT, false));
        assert!(msg.contains("did not answer"), "got: {msg}");
    }

    #[test]
    fn linking_restarts_the_deadline_and_faults_as_no_flow() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(t0, Some("mic".into()));
        let connected = t0 + SILENCE_TIMEOUT - MS;
        sup.linking(connected);
        assert_eq!(sup.tick(t0 + SILENCE_TIMEOUT, false), None);
        assert_eq!(sup.tick(connected + SILENCE_TIMEOUT - MS, false), None);
        let msg = fault_message(sup.tick(connected + SILENCE_TIMEOUT, false));
        assert_eq!(msg, no_flow_message(Some("mic")));
    }

    #[test]
    fn stop_before_the_stream_is_wired_is_a_fault() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(t0, None);
        assert!(stopped_before_wired(sup.tick(t0, true)));
        sup.linking(t0);
        assert!(stopped_before_wired(sup.tick(t0, true)));
        // Stop wins over an expired deadline: the user asked to end.
        assert!(stopped_before_wired(sup.tick(t0 + SILENCE_TIMEOUT, true)));
    }

    /// A tap released while the device resumes: wired, no audio yet.
    #[test]
    fn stop_after_wiring_before_audio_is_a_clean_empty_end() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(t0, None);
        sup.linking(t0);
        sup.wired();
        assert_eq!(sup.tick(t0 + MS, true), Some(Ending::Clean));
        // Wiring proves nothing about flow: the link deadline still holds.
        assert_eq!(sup.tick(t0 + SILENCE_TIMEOUT - MS, false), None);
        let msg = fault_message(sup.tick(t0 + SILENCE_TIMEOUT, false));
        assert_eq!(msg, no_flow_message(None));
    }

    #[test]
    fn capture_faults_when_delivery_stalls() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(t0, None);
        sup.linking(t0);
        sup.delivered(t0 + MS);
        let last = t0 + 5 * SILENCE_TIMEOUT;
        sup.delivered(last);
        assert_eq!(sup.tick(last + SILENCE_TIMEOUT - MS, false), None);
        let msg = fault_message(sup.tick(last + SILENCE_TIMEOUT, false));
        assert_eq!(msg, no_flow_message(None));
    }

    #[test]
    fn stop_after_audio_is_a_clean_end() {
        let t0 = Instant::now();
        let mut sup = Supervisor::new(t0, None);
        sup.linking(t0);
        sup.delivered(t0 + MS);
        assert_eq!(sup.tick(t0 + MS, true), Some(Ending::Clean));
        assert_eq!(sup.tick(t0 + MS, false), None);
    }

    const CYCLE: u64 = 1024;
    const GRAPH: (u32, u32) = (1, 48_000);

    /// Feeds whole 1024-tick cycles as a 48 kHz graph resampled to 16 kHz
    /// delivers them: 341, 341, 342 frames.
    fn run(c: &mut Continuity, ticks: &mut u64, cycles: u64) -> Option<Duration> {
        let mut lost = None;
        for _ in 0..cycles {
            *ticks += CYCLE;
            let frames = if *ticks / CYCLE % 3 == 0 { 342 } else { 341 };
            lost = lost.or(c.delivered(*ticks, GRAPH, frames));
        }
        lost
    }

    #[test]
    fn resampled_delivery_on_every_cycle_loses_nothing() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 7;
        assert_eq!(run(&mut c, &mut ticks, 100_000), None);
    }

    #[test]
    fn a_missed_cycle_past_the_tolerance_is_lost_audio() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 10);
        ticks += CYCLE;
        let lost = run(&mut c, &mut ticks, 1).expect("one 21 ms cycle lost");
        assert!(
            lost >= LOSS_TOLERANCE && lost < Duration::from_millis(22),
            "{lost:?}"
        );
    }

    #[test]
    fn small_losses_accumulate_until_they_cross_the_tolerance() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        let quantum = 256;
        let miss = |c: &mut Continuity, ticks: &mut u64| {
            *ticks += 2 * quantum;
            c.delivered(*ticks, GRAPH, 85)
        };
        assert_eq!(c.delivered(ticks, GRAPH, 85), None);
        for _ in 0..3 {
            assert_eq!(miss(&mut c, &mut ticks), None);
        }
        assert!(miss(&mut c, &mut ticks).is_some());
    }

    #[test]
    fn a_surplus_does_not_bank_credit_against_later_loss() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        ticks += 1;
        assert_eq!(c.delivered(ticks, GRAPH, 5_000), None);
        ticks += CYCLE;
        assert!(run(&mut c, &mut ticks, 1).is_some());
    }

    #[test]
    fn a_clock_rebase_delivers_without_ticks_and_is_not_loss() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        assert_eq!(c.delivered(ticks, GRAPH, 341), None);
        assert_eq!(run(&mut c, &mut ticks, 30), None);
    }

    #[test]
    fn a_clock_without_a_rate_reports_nothing() {
        let mut c = Continuity::new(16_000);
        assert_eq!(c.delivered(0, (0, 0), 341), None);
        assert_eq!(c.delivered(CYCLE, (0, 0), 341), None);
    }

    #[test]
    fn a_restart_forgets_the_gap_it_spans() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        c.restart();
        ticks += 48_000 * 10;
        assert_eq!(run(&mut c, &mut ticks, 30), None);
    }

    #[test]
    fn ticks_follow_the_graph_rate() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0u64;
        let rate = (1, 44_100);
        // 1024 ticks at 44.1 kHz are 371.52 frames at 16 kHz.
        let mut owed = 0.0f64;
        for _ in 0..10_000 {
            ticks += CYCLE;
            owed += 1024.0 * 16_000.0 / 44_100.0;
            let frames = owed.floor();
            owed -= frames;
            assert_eq!(c.delivered(ticks, rate, frames as u64), None);
        }
        ticks += 2 * CYCLE;
        assert!(c.delivered(ticks, rate, 372).is_some());
    }

    /// The no-source fault message is user-facing and actionable (names the
    /// missing session manager), content-free, and includes the target when
    /// one was requested.
    #[test]
    fn link_wait_timeout_message() {
        let plain = no_source_message(None);
        assert!(plain.contains("session manager"), "got: {plain}");
        assert!(plain.contains("no audio source"), "got: {plain}");
        let targeted = no_source_message(Some("alsa_input.pci-0000_00_1f.3.analog-stereo"));
        assert!(targeted.contains("alsa_input.pci-0000_00_1f.3.analog-stereo"));
        assert!(targeted.contains("session manager"));
    }

    /// The dead-stream watchdog message is likewise user-facing, content-free,
    /// and names the target when one was requested.
    #[test]
    fn watchdog_message() {
        let plain = no_flow_message(None);
        assert!(plain.contains("no audio is flowing"), "got: {plain}");
        let targeted = no_flow_message(Some("alsa_input.usb"));
        assert!(targeted.contains("alsa_input.usb"));
        assert!(targeted.contains("no audio is flowing"));
    }

    /// T026: an empty channel selection is rejected up front (C7) — never a
    /// silent mis-capture.
    #[tokio::test]
    async fn empty_channel_selection_is_rejected() {
        let fmt = CoreFormat::default();
        let source = CaptureSource::builder(fmt)
            .channels(vec![])
            .backend(Box::new(PipeWireBackend::new()))
            .build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        assert!(chunks.is_empty());
        assert!(matches!(fault, Some(CaptureError::Backend(_))));
    }

    /// T025 (pure unit): pick + downmix interleaved S16 frames by channel index.
    #[test]
    fn select_channels_downmix_to_mono() {
        // 4-channel frame: [100, 200, 300, 400], select ch2+ch3 → mono avg=350.
        let frame: Vec<u8> = [100i16, 200, 300, 400]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = select_channels_s16(&frame, 4, &[2, 3], 1);
        assert_eq!(out.len(), 2);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 350);
    }

    #[test]
    fn select_channels_identity_stereo() {
        // 4-channel frame, select ch0+ch2 → stereo [100, 300].
        let frame: Vec<u8> = [100i16, 200, 300, 400]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = select_channels_s16(&frame, 4, &[0, 2], 2);
        assert_eq!(out.len(), 4);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 100);
        assert_eq!(i16::from_le_bytes([out[2], out[3]]), 300);
    }

    #[test]
    fn select_channels_drops_trailing_partial_frame() {
        // 4 bytes = 1 frame of stereo + a dangling nothing; in=2, select ch1.
        let data: Vec<u8> = [10i16, 20].iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = select_channels_s16(&data, 2, &[1], 1);
        assert_eq!(out.len(), 2);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 20);
    }

    #[test]
    fn select_channels_out_of_range_index_contributes_silence() {
        // Select ch5 from a stereo frame: no valid source → 0.
        let frame: Vec<u8> = [100i16, 200].iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = select_channels_s16(&frame, 2, &[5], 1);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 0);
    }
}
