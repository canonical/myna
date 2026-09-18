//! [`PipeWireBackend`] (plan T52) — native live capture via `pipewire-rs`,
//! behind the [`CaptureBackend`] seam. No subprocess: a dedicated capture loop
//! thread owns the PipeWire loop and a capture `Stream` connected with
//! `RT_PROCESS`, so `process` runs on PipeWire's realtime data thread. There
//! it only copies or downmixes samples into a preallocated lock-free ring and
//! latches losses in atomics ([`DataPath`]). The loop thread drains that ring
//! into [`Producer::push`] and owns everything else: chunking, stats, health,
//! the phase deadlines and teardown.
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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
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
    stream::{Stream, StreamFlags, StreamRc, StreamState},
};

use crate::backend::{CaptureBackend, CaptureSpec, Producer};

/// How often the loop thread drains the realtime ring and checks the
/// [`StopHandle`] and the phase deadline: at most this much latency on each
/// chunk, and well inside the ~250 ms stop/abort promptness contract (FR-012).
const POLL: Duration = Duration::from_millis(20);

/// Audio the realtime ring holds while the loop thread is not draining it.
/// The loop thread is an ordinary thread waking every [`POLL`], and even with
/// every core oversubscribed several times over the scheduler runs it within
/// tens of milliseconds: two orders of magnitude of headroom, for 64 KB at
/// 16 kHz mono (384 KB at 48 kHz stereo). Starving it longer overflows the
/// ring, which faults.
const REALTIME_BUFFER: Duration = Duration::from_secs(2);

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

/// Deficit tolerated before it is even a candidate for lost audio: under one
/// default quantum (1024 frames at 48 kHz), far above the sub-frame resampler
/// jitter. It is also the floor under the phase credit in [`Continuity`].
const LOSS_TOLERANCE: Duration = Duration::from_millis(20);

/// Audio that must arrive after a deficit appears before the deficit counts as
/// lost. Delivery runs in and out of phase with the stream clock by a cycle,
/// so a deficit is routinely repaid by the very next callback; audio the graph
/// overwrote is never repaid. Nine default quanta (21.3 ms each at 48 kHz),
/// and five times the largest repaid deficit the A1 capture load matrix
/// recorded (37 ms): it defers a real fault by at most this much audio,
/// never hides it.
const LOSS_CONFIRM: Duration = Duration::from_millis(200);

/// How far a delivery has to move before it is the graph changing its cycle
/// rather than the resampler rounding one up and the next down (a frame either
/// way). Quanta move by factors of two.
const CYCLE_CHANGE: f64 = 1.25;

/// Audio the graph produced for this stream but never handed over. PipeWire
/// overwrites the buffer of each graph cycle the data thread misses (an xrun)
/// without reporting it, but the stream clock still advances for every cycle,
/// so clock minus delivered frames is the loss.
///
/// Delivery and the stream clock run in and out of phase by a cycle: on a
/// loaded 48 kHz graph a cycle's tick advance and its buffer reach `process`
/// in two callbacks, one reporting two quanta of clock with one quantum of
/// audio and the next the missing quantum with `pw_time.ticks` unmoved. So
/// every delivery counts, whether or not the clock moved; the account may run
/// one graph cycle ahead of the clock; and a deficit is loss only once
/// [`LOSS_CONFIRM`] of later audio has failed to repay it.
///
/// The account holds only within one clock domain and one graph cycle. Both
/// can change without the stream leaving `Streaming` (a rate switch, a quantum
/// change), and the phase either leaves behind belongs to a geometry that no
/// longer exists, so either restarts the account rather than being charged as
/// loss.
struct Continuity {
    rate: f64,
    /// The clock domain `last_ticks` was read in.
    domain: Option<(u32, u32)>,
    last_ticks: Option<u64>,
    /// Frames owed. Negative is the account running ahead of the clock, and is
    /// bounded to one cycle so no surplus can pay for a real loss.
    owed: f64,
    /// Frames in one graph cycle: the smallest tick advance seen since the
    /// last restart, since a missed cycle only ever makes an advance larger.
    cycle: f64,
    /// Audio delivered while `owed` has been over [`LOSS_TOLERANCE`], `None`
    /// while it is under.
    unrepaid: Option<f64>,
    /// Frames in one callback's delivery, the graph's cycle as handed over,
    /// and how many deliveries running have disagreed with it.
    delivery: f64,
    disagreed: u32,
}

impl Continuity {
    fn new(rate: u32) -> Self {
        Self {
            rate: rate as f64,
            domain: None,
            last_ticks: None,
            owed: 0.0,
            cycle: 0.0,
            unrepaid: None,
            delivery: 0.0,
            disagreed: 0,
        }
    }

    /// The stream changed state; nothing is owed across a pause. The cycle
    /// measurement is the graph's, not the account's, and outlives this.
    fn restart(&mut self) {
        self.last_ticks = None;
        self.owed = 0.0;
        self.cycle = 0.0;
        self.unrepaid = None;
    }

    /// Fold one delivery into the cycle measurement, reporting a graph cycle
    /// that changed. The graph hands one cycle over per callback, so the size
    /// of a delivery is the cycle it ran at; a burst of buffers or a short
    /// last one moves a single delivery, a quantum change moves every one
    /// after it, so it takes two in a row to count. An empty delivery is no
    /// cycle at all.
    fn measure(&mut self, frames: u64) -> bool {
        let frames = frames as f64;
        if frames == 0.0 {
            return false;
        }
        if self.delivery == 0.0 {
            self.delivery = frames;
            return false;
        }
        if frames <= self.delivery * CYCLE_CHANGE && frames * CYCLE_CHANGE >= self.delivery {
            self.disagreed = 0;
            return false;
        }
        self.disagreed += 1;
        if self.disagreed < 2 {
            return false;
        }
        self.disagreed = 0;
        self.delivery = frames;
        true
    }

    /// `frames` arrived at stream clock `ticks` (in `graph_rate` units).
    /// Returns the loss once a deficit past [`LOSS_TOLERANCE`] has outlived
    /// [`LOSS_CONFIRM`] of later audio.
    fn delivered(&mut self, ticks: u64, graph_rate: (u32, u32), frames: u64) -> Option<Duration> {
        // A rateless report (`denom` 0) says nothing about the domain, and is
        // written off below rather than restarting the account.
        let domain = (graph_rate.1 != 0).then_some(graph_rate);
        if self.measure(frames) || (domain.is_some() && domain != self.domain) {
            self.restart();
            self.domain = domain;
        }
        let last = self.last_ticks.replace(ticks)?;
        let (num, denom) = graph_rate;
        // A clock that did not move (a rebase, or the second callback of a
        // split cycle) produced nothing over this interval. Its frames still
        // arrived, so they still count.
        let expected = if ticks > last && denom != 0 {
            (ticks - last) as f64 * num as f64 * self.rate / denom as f64
        } else {
            0.0
        };
        if expected > 0.0 && (self.cycle == 0.0 || expected < self.cycle) {
            self.cycle = expected;
        }
        let tolerance = LOSS_TOLERANCE.as_secs_f64() * self.rate;
        self.owed = (self.owed + expected - frames as f64).max(-self.cycle.max(tolerance));
        if self.owed <= tolerance {
            self.unrepaid = None;
            return None;
        }
        match self.unrepaid.as_mut() {
            // The delivery that opened the deficit is no evidence against it.
            None => {
                self.unrepaid = Some(0.0);
                None
            }
            Some(arrived) => {
                *arrived += frames as f64;
                (*arrived >= LOSS_CONFIRM.as_secs_f64() * self.rate)
                    .then(|| Duration::from_secs_f64(self.owed / self.rate))
            }
        }
    }

    /// The deficit standing right now, for a capture that is ending: no later
    /// audio can repay it any more, so [`LOSS_CONFIRM`] would never run out.
    /// Reported only past what the phase of one graph cycle and the tolerance
    /// together explain, which no split cycle can reach; a smaller deficit
    /// stays unreportable at the tail, as it is mid-capture.
    fn outstanding(&self) -> Option<Duration> {
        let tolerance = LOSS_TOLERANCE.as_secs_f64() * self.rate;
        (self.owed > self.cycle.max(tolerance) + tolerance)
            .then(|| Duration::from_secs_f64(self.owed / self.rate))
    }
}

/// The fault when graph cycles went by without reaching the capture thread.
fn lost_audio_message(lost: Duration) -> String {
    format!(
        "{} ms of audio was lost: the capture thread could not keep up, the system may be overloaded",
        lost.as_millis()
    )
}

/// The fault when the loop thread fell too far behind the data thread.
fn overflow_message() -> String {
    format!(
        "the capture buffer overflowed: audio waited more than {} s to be read, the system may be overloaded",
        REALTIME_BUFFER.as_secs()
    )
}

/// Audio the data thread could not hand over.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Loss {
    /// Graph cycles went by without their buffers reaching `process`.
    Missed(Duration),
    /// The realtime ring was full.
    Overflow,
}

impl Loss {
    fn into_error(self) -> CaptureError {
        CaptureError::Backend(match self {
            Loss::Missed(lost) => lost_audio_message(lost),
            Loss::Overflow => overflow_message(),
        })
    }
}

/// The only state `process` shares with the loop thread.
#[derive(Default)]
struct Shared {
    /// Bumped by the loop thread on every stream state change.
    epoch: AtomicU32,
    /// The data thread's one [`Loss`]: `0` none, `u64::MAX` an overflow,
    /// otherwise the microseconds missed.
    loss: AtomicU64,
    /// The deficit standing at the last delivery, in the same code, which
    /// only a capture that has ended reads: mid-capture it is a candidate for
    /// loss, not loss.
    unconfirmed: AtomicU64,
}

impl Shared {
    const OVERFLOW: u64 = u64::MAX;

    fn latch(&self, loss: Loss) {
        let code = match loss {
            Loss::Overflow => Self::OVERFLOW,
            Loss::Missed(lost) => micros(lost),
        };
        self.loss.store(code, Ordering::Release);
    }

    fn loss(&self) -> Option<Loss> {
        match self.loss.load(Ordering::Acquire) {
            0 => None,
            Self::OVERFLOW => Some(Loss::Overflow),
            micros => Some(Loss::Missed(Duration::from_micros(micros))),
        }
    }

    fn hold(&self, deficit: Option<Duration>) {
        self.unconfirmed
            .store(deficit.map_or(0, micros), Ordering::Release);
    }

    fn unconfirmed(&self) -> Option<Loss> {
        match self.unconfirmed.load(Ordering::Acquire) {
            0 => None,
            micros => Some(Loss::Missed(Duration::from_micros(micros))),
        }
    }
}

/// A duration as the microsecond code the [`Shared`] atomics carry: never `0`
/// (nothing) nor [`Shared::OVERFLOW`].
fn micros(lost: Duration) -> u64 {
    u64::try_from(lost.as_micros())
        .unwrap_or(u64::MAX)
        .clamp(1, Shared::OVERFLOW - 1)
}

/// How the data thread turns a stream buffer into the negotiated format.
enum Mix {
    /// The stream already carries the negotiated channels.
    Passthrough { frame_bytes: usize },
    /// Pick channel indices from an `in_channels`-wide stream; each bucket of
    /// indices averages into one output channel (§9, T025).
    Select {
        in_channels: usize,
        buckets: Vec<Vec<usize>>,
    },
}

impl Mix {
    fn new(selection: Option<&[u8]>, in_channels: usize, out_channels: usize) -> Self {
        match selection {
            Some(selected) if !selected.is_empty() && out_channels > 0 => Mix::Select {
                in_channels: in_channels.max(1),
                buckets: channel_buckets(selected, out_channels),
            },
            _ => Mix::Passthrough {
                frame_bytes: in_channels.max(1) * 2,
            },
        }
    }

    /// Whole stream frames in `samples`.
    fn frames(&self, samples: &[u8]) -> u64 {
        let frame_bytes = match self {
            Mix::Passthrough { frame_bytes } => *frame_bytes,
            Mix::Select { in_channels, .. } => in_channels * 2,
        };
        (samples.len() / frame_bytes) as u64
    }
}

/// Everything `process` touches, owned by the realtime data thread. It reaches
/// the loop thread only through the ring and [`Shared`]; none of it allocates,
/// locks or blocks.
struct DataPath {
    ring: rtrb::Producer<u8>,
    shared: Arc<Shared>,
    mix: Mix,
    continuity: Continuity,
    epoch: u32,
    /// A loss ends capture: nothing after it is handed over.
    lost: bool,
}

impl DataPath {
    fn new(ring: rtrb::Producer<u8>, shared: Arc<Shared>, mix: Mix, rate: u32) -> Self {
        Self {
            ring,
            shared,
            mix,
            continuity: Continuity::new(rate),
            epoch: 0,
            lost: false,
        }
    }

    /// Hand one non-empty buffer to the loop thread, whole or not at all.
    /// Returns its frames.
    fn deliver(&mut self, samples: &[u8]) -> u64 {
        let frames = self.mix.frames(samples);
        if self.lost {
            return frames;
        }
        let written = match &self.mix {
            Mix::Passthrough { .. } => self.ring.push_entire_slice(samples).is_ok(),
            Mix::Select {
                in_channels,
                buckets,
            } => match self
                .ring
                .write_chunk_uninit(frames as usize * buckets.len() * 2)
            {
                Ok(chunk) => {
                    chunk.fill_from_iter(downmix_s16(samples, *in_channels, buckets));
                    true
                }
                Err(_) => false,
            },
        };
        if !written {
            self.lose(Loss::Overflow);
        }
        frames
    }

    /// `frames` arrived with the stream clock at `ticks`.
    fn account(&mut self, ticks: u64, graph_rate: (u32, u32), frames: u64) {
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.continuity.restart();
        }
        match self.continuity.delivered(ticks, graph_rate, frames) {
            Some(lost) => self.lose(Loss::Missed(lost)),
            // A deficit no confirmation window has run out on yet, for the
            // loop thread to read if capture ends before one can.
            None if !self.lost => self.shared.hold(self.continuity.outstanding()),
            None => {}
        }
    }

    fn lose(&mut self, loss: Loss) {
        if !std::mem::replace(&mut self.lost, true) {
            self.shared.latch(loss);
        }
    }
}

thread_local! {
    /// Set on the capture loop thread, which `process` must never run on.
    static ON_CAPTURE_LOOP: Cell<bool> = const { Cell::new(false) };
}

/// The `process` callback. The `Send` bound proves at compile time that it
/// shares nothing unsynchronized with the loop thread.
fn realtime_process(mut path: DataPath) -> impl FnMut(&Stream, &mut ()) + Send + 'static {
    move |stream, _| {
        debug_assert!(
            !ON_CAPTURE_LOOP.with(Cell::get),
            "process must run on the realtime data thread"
        );
        let mut frames = 0u64;
        while let Some(mut buffer) = stream.dequeue_buffer() {
            let Some(data) = buffer.datas_mut().first_mut() else {
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
            if !slice.is_empty() {
                frames += path.deliver(slice);
            }
        }
        if frames == 0 {
            return;
        }
        if let Ok(time) = stream.time() {
            let rate = time.rate();
            path.account(time.ticks(), (rate.num, rate.denom), frames);
        }
    }
}

/// The loop thread's end of the data path.
struct Drain {
    ring: rtrb::Consumer<u8>,
    shared: Arc<Shared>,
}

/// What one [`Drain::run`] found.
#[derive(Debug, PartialEq)]
struct Drained {
    delivered: bool,
    /// The consumer has gone (abort).
    abandoned: bool,
    /// The data thread lost audio; everything it buffered before has moved.
    loss: Option<Loss>,
}

impl Drain {
    fn run(&mut self, producer: &mut Producer) -> Drained {
        // Read first: whatever was written before the latch is already visible.
        let loss = self.shared.loss();
        let available = self.ring.slots();
        let mut abandoned = false;
        if available > 0 {
            if let Ok(chunk) = self.ring.read_chunk(available) {
                let (head, tail) = chunk.as_slices();
                let mut bytes = BytesMut::with_capacity(available);
                bytes.extend_from_slice(head);
                bytes.extend_from_slice(tail);
                chunk.commit_all();
                abandoned = !producer.push(bytes.freeze());
            }
        }
        Drained {
            delivered: available > 0,
            abandoned,
            loss,
        }
    }

    /// The deficit the data thread ended on, once it is quiesced and no later
    /// audio can repay it. Only a capture that is over may read this.
    fn unconfirmed(&self) -> Option<Loss> {
        self.shared.unconfirmed()
    }
}

/// The realtime ring for `format`, every page touched here so the data thread
/// never takes a page fault on first write.
fn realtime_ring(format: myna_core::AudioFormat) -> (rtrb::Producer<u8>, rtrb::Consumer<u8>) {
    let capacity = (format.bytes_per_second() as f64 * REALTIME_BUFFER.as_secs_f64()) as usize;
    let (mut tx, mut rx) = rtrb::RingBuffer::new(capacity.max(1));
    if let Ok(chunk) = tx.write_chunk_uninit(tx.slots()) {
        chunk.fill_from_iter(std::iter::repeat(0));
    }
    if let Ok(chunk) = rx.read_chunk(rx.slots()) {
        chunk.commit_all();
    }
    (tx, rx)
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
    ON_CAPTURE_LOOP.with(|on| on.set(true));
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

    let (ring, consumer) = realtime_ring(spec.format);
    let shared = Arc::new(Shared::default());
    let drain = Rc::new(RefCell::new(Drain {
        ring: consumer,
        shared: shared.clone(),
    }));

    // Poll timer: drains the realtime ring, and observes stop, abort and the
    // phase deadlines in every phase, from discovery on (FR-012, SC-009).
    let timer = main_loop.loop_().add_timer({
        let supervisor = supervisor.clone();
        let stop = spec.stop.clone();
        let end = end.clone();
        let producer = producer.clone();
        let drain = drain.clone();
        move |_| {
            let now = Instant::now();
            if let Some(producer) = producer.borrow_mut().as_mut() {
                let drained = drain.borrow_mut().run(producer);
                if drained.delivered {
                    supervisor.borrow_mut().delivered(now);
                }
                if drained.abandoned {
                    // Consumer gone (abort) → end promptly (FR-011).
                    end(Ending::Clean);
                }
                if let Some(loss) = drained.loss {
                    end(Ending::Fault(loss.into_error()));
                }
            }
            if let Some(why) = supervisor.borrow().tick(now, stop.is_stopped()) {
                end(why);
            }
        }
    });
    let _ = timer.update_timer(Some(POLL), Some(POLL)).into_result();

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

    let selection = spec.channels.clone();
    let stream_channels = stream_channels(selection.as_deref(), spec.format.channels);

    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let end = end.clone();
            let target = spec.target.clone();
            let supervisor = supervisor.clone();
            let shared = shared.clone();
            move |_stream, _ud, _old, new| {
                shared.epoch.fetch_add(1, Ordering::AcqRel);
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
        .register();
    // A listener of its own: pipewire-rs lends each callback its whole
    // listener mutably, and this one runs on the data thread.
    let path = DataPath::new(
        ring,
        shared,
        Mix::new(
            selection.as_deref(),
            stream_channels as usize,
            spec.format.channels as usize,
        ),
        spec.format.sample_rate_hz,
    );
    let rt_listener = stream
        .add_local_listener_with_user_data(())
        .process(realtime_process(path))
        .register();
    let (listener, rt_listener) = match (listener, rt_listener) {
        (Ok(l), Ok(rt)) => (l, rt),
        (Err(e), _) | (_, Err(e)) => {
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
    let mut flags = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
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

    // Disconnecting removes the node from the data loop under its lock, so
    // `process` has returned for good before the ring's last audio drains,
    // exactly once, ahead of the terminal outcome. The PipeWire objects drop
    // on return.
    let _ = stream.disconnect();
    drop(rt_listener);
    drop(listener);
    drop(timer);
    let loss = producer.borrow_mut().as_mut().and_then(|producer| {
        let mut drain = drain.borrow_mut();
        // No audio can repay a deficit now, so the confirmation window that
        // defers one mid-capture would never run out: read it here or never.
        drain.run(producer).loss.or_else(|| drain.unconfirmed())
    });
    let why = ending.borrow_mut().take();
    final_ending(why, loss).and_then(Ending::into_fault)
}

/// How capture ended, given the audio the data thread lost before it was
/// quiesced: a loss outranks a clean end, never an earlier fault.
fn final_ending(why: Option<Ending>, loss: Option<Loss>) -> Option<Ending> {
    match (why, loss) {
        (Some(Ending::Fault(err)), _) => Some(Ending::Fault(err)),
        (_, Some(loss)) => Some(Ending::Fault(loss.into_error())),
        (why, None) => why,
    }
}

/// Channels to ask the graph for (§9): with a selection, enough to contain
/// every selected index, which the process callback then picks and downmixes
/// to the negotiated count (T025); otherwise the negotiated count itself.
fn stream_channels(selection: Option<&[u8]>, negotiated: u8) -> u32 {
    match selection {
        Some(indices) => indices.iter().max().map_or(1, |&max| max as u32 + 1),
        None => negotiated as u32,
    }
}

/// Which selected channel indices feed each output channel: all of them for
/// mono out, 1:1 when the counts match, otherwise round-robin (best-effort).
fn channel_buckets(selected: &[u8], out_channels: usize) -> Vec<Vec<usize>> {
    let mut buckets = vec![Vec::new(); out_channels];
    for (n, &ch) in selected.iter().enumerate() {
        buckets[n % out_channels].push(ch as usize);
    }
    buckets
}

/// Interleaved S16LE frames `in_channels` wide, each bucket of channel indices
/// averaged into one output sample. A trailing partial frame is dropped; an
/// index past the stream contributes nothing, and a bucket without a valid
/// index is silence. Lazy, so the data thread writes it straight into the ring.
fn downmix_s16<'a>(
    data: &'a [u8],
    in_channels: usize,
    buckets: &'a [Vec<usize>],
) -> impl Iterator<Item = u8> + 'a {
    data.chunks_exact(in_channels * 2).flat_map(move |frame| {
        buckets.iter().flat_map(move |bucket| {
            let (sum, count) = bucket.iter().filter(|&&ch| ch < in_channels).fold(
                (0i32, 0i32),
                |(sum, count), &ch| {
                    let sample = i16::from_le_bytes([frame[2 * ch], frame[2 * ch + 1]]);
                    (sum + sample as i32, count + 1)
                },
            );
            let mixed = if count > 0 { (sum / count) as i16 } else { 0 };
            mixed.to_le_bytes()
        })
    })
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

    /// Deliver [`LOSS_CONFIRM`] of audio that neither gains nor loses a frame,
    /// which is what turns an outstanding deficit into a reported loss. One
    /// graph cycle of `frames` per callback, as the data thread sees it.
    fn without_repaying(
        c: &mut Continuity,
        ticks: &mut u64,
        (num, denom): (u32, u32),
        frames: u64,
    ) -> Option<Duration> {
        let mut lost = None;
        let mut sent = 0;
        while sent < (LOSS_CONFIRM.as_secs_f64() * 16_000.0) as u64 {
            *ticks += frames * denom as u64 / (num as u64 * 16_000);
            lost = lost.or(c.delivered(*ticks, (num, denom), frames));
            sent += frames;
        }
        lost
    }

    /// The observed false positive (loaded 48 kHz graph, quantum 1024): a
    /// graph cycle's tick advance and its buffer reach `process` in two
    /// callbacks: the first reports two quanta of clock with one quantum of
    /// audio, the next reports the missing quantum with the clock unmoved.
    /// Nothing was lost, so nothing may fault.
    #[test]
    fn a_cycle_split_across_two_callbacks_is_not_lost_audio() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 10);
        // Two quanta of clock, one quantum of audio.
        ticks += CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None);
        // The rest of it, with the clock standing still.
        assert_eq!(c.delivered(ticks, GRAPH, 342), None);
        assert_eq!(without_repaying(&mut c, &mut ticks, GRAPH, 341), None);
    }

    /// The same split the other way round: the buffer arrives before the tick
    /// advance that accounts for it. The account may run one cycle ahead of
    /// the stream clock.
    #[test]
    fn a_buffer_ahead_of_its_tick_advance_is_not_lost_audio() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 10);
        assert_eq!(c.delivered(ticks, GRAPH, 342), None);
        ticks += CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None);
        assert_eq!(without_repaying(&mut c, &mut ticks, GRAPH, 341), None);
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
        assert_eq!(run(&mut c, &mut ticks, 1), None, "not before later audio");
        let lost = without_repaying(&mut c, &mut ticks, GRAPH, 341).expect("one 21 ms cycle lost");
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
        for _ in 0..4 {
            assert_eq!(miss(&mut c, &mut ticks), None);
        }
        assert!(without_repaying(&mut c, &mut ticks, GRAPH, 85).is_some());
    }

    /// The phase credit is one graph cycle, measured off the clock rather than
    /// assumed: at a 2048 quantum a cycle is 42.7 ms, twice the tolerance.
    #[test]
    fn a_long_quantum_may_still_slip_a_whole_cycle_out_of_phase() {
        let mut c = Continuity::new(16_000);
        let long = 2 * CYCLE;
        let mut ticks = 0;
        for _ in 0..10 {
            ticks += long;
            assert_eq!(c.delivered(ticks, GRAPH, 683), None);
        }
        // A whole cycle arrives before the tick advance that accounts for it.
        assert_eq!(c.delivered(ticks, GRAPH, 683), None);
        ticks += 2 * long;
        assert_eq!(c.delivered(ticks, GRAPH, 683), None);
        assert_eq!(without_repaying(&mut c, &mut ticks, GRAPH, 683), None);
    }

    /// And it is the smallest advance: an account opened across a missed cycle
    /// must not bank two cycles of credit.
    #[test]
    fn a_missed_cycle_does_not_widen_the_phase_credit() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        assert_eq!(c.delivered(ticks, GRAPH, 341), None);
        ticks += 2 * CYCLE;
        assert_eq!(c.delivered(ticks, GRAPH, 683), None);
        run(&mut c, &mut ticks, 10);
        assert_eq!(c.delivered(ticks, GRAPH, 5_000), None);
        ticks += 2 * CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None);
        assert!(without_repaying(&mut c, &mut ticks, GRAPH, 341).is_some());
    }

    /// One cycle of surplus is credit - delivery and the clock slip that far
    /// out of phase - and the rest of it is gone, so it cannot pay for a loss.
    #[test]
    fn a_surplus_banks_at_most_one_cycle_against_a_later_loss() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        ticks += 1;
        assert_eq!(c.delivered(ticks, GRAPH, 5_000), None);
        ticks += 3 * CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None);
        assert!(without_repaying(&mut c, &mut ticks, GRAPH, 341).is_some());
    }

    #[test]
    fn a_clock_rebase_delivers_without_ticks_and_is_not_loss() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        assert_eq!(c.delivered(ticks, GRAPH, 341), None);
        assert_eq!(run(&mut c, &mut ticks, 30), None);
        assert_eq!(without_repaying(&mut c, &mut ticks, GRAPH, 341), None);
    }

    /// A rebase reports no tick advance, but its buffer still arrived: those
    /// frames repay what is owed, and bank at most one cycle beyond it.
    #[test]
    fn a_clock_rebase_credits_the_audio_it_delivered_and_no_more() {
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
        assert_eq!(c.delivered(ticks, GRAPH, 341), None);
        for _ in 0..6 {
            assert_eq!(miss(&mut c, &mut ticks), None);
        }
        assert!(without_repaying(&mut c, &mut ticks, GRAPH, 85).is_some());
    }

    #[test]
    fn exactly_the_tolerance_is_not_yet_a_loss() {
        let same = (2, 32_000);
        // 320 frames owed is the tolerance exactly, and no amount of later
        // audio turns it into a loss.
        let mut c = Continuity::new(16_000);
        let mut ticks = 320;
        assert_eq!(c.delivered(0, same, 1), None);
        assert_eq!(c.delivered(ticks, same, 0), None);
        assert_eq!(without_repaying(&mut c, &mut ticks, same, 1), None);
        // One frame more is.
        let mut c = Continuity::new(16_000);
        let mut ticks = 321;
        assert_eq!(c.delivered(0, same, 1), None);
        assert_eq!(c.delivered(ticks, same, 0), None);
        assert!(without_repaying(&mut c, &mut ticks, same, 1).is_some());
    }

    #[test]
    fn a_clock_without_a_rate_reports_nothing() {
        let mut c = Continuity::new(16_000);
        assert_eq!(c.delivered(0, (0, 0), 341), None);
        assert_eq!(c.delivered(CYCLE, (0, 0), 341), None);
    }

    /// A rateless report says nothing about what the graph produced, so it
    /// neither owes anything nor writes off what is already owed.
    #[test]
    fn a_clock_without_a_rate_does_not_wipe_the_account() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 3);
        ticks += 4 * CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None);
        ticks += CYCLE;
        assert_eq!(c.delivered(ticks, (0, 0), 341), None);
        assert!(without_repaying(&mut c, &mut ticks, GRAPH, 341).is_some());
    }

    /// The graph clock can change rate under a running stream, which is no
    /// stream state change and so no restart. Ticks in the new domain count
    /// something else entirely: the old ones cannot be subtracted from them.
    #[test]
    fn a_graph_clock_rate_change_starts_a_new_account() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 40);
        // The same stream, now counted by a 44.1 kHz clock from its own base.
        let rate = (1, 44_100);
        let mut ticks = 44_100;
        assert_eq!(c.delivered(ticks, rate, 372), None);
        for _ in 0..40 {
            ticks += CYCLE;
            assert_eq!(c.delivered(ticks, rate, 372), None, "a rate change is loss");
        }
        assert_eq!(without_repaying(&mut c, &mut ticks, rate, 372), None);
    }

    /// [`LOSS_CONFIRM`] cannot run out at the end of a capture, so a deficit
    /// bigger than the phase of one cycle and the tolerance together can
    /// explain is reported as the tail of it.
    #[test]
    fn a_deficit_no_audio_can_repay_is_reported_at_the_end_of_a_capture() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 40);
        assert_eq!(c.outstanding(), None);
        // Half a second of cycles the data thread never saw.
        ticks += 24_000;
        assert_eq!(c.delivered(ticks, GRAPH, 341), None, "not confirmed yet");
        let lost = c.outstanding().expect("the tail of the capture");
        assert!(
            lost > Duration::from_millis(470) && lost < Duration::from_millis(500),
            "{lost:?}"
        );
    }

    /// The phase the account ends on is not a loss: one cycle either way is
    /// how delivery and the stream clock run, at any quantum.
    #[test]
    fn the_phase_a_capture_ends_on_is_not_reported_as_loss() {
        for (quantum, frames) in [(CYCLE, 341), (4 * CYCLE, 1_365)] {
            let mut c = Continuity::new(16_000);
            let mut ticks = 0;
            for _ in 0..40 {
                ticks += quantum;
                assert_eq!(c.delivered(ticks, GRAPH, frames), None);
            }
            // A whole cycle of clock arrives without its buffer, and capture
            // ends before the next callback could repay it.
            ticks += 2 * quantum;
            assert_eq!(c.delivered(ticks, GRAPH, frames), None);
            assert_eq!(c.outstanding(), None, "quantum {quantum}");
        }
    }

    /// A single outsized delivery is a burst of buffers, not a graph running
    /// a longer cycle, and must not forgive what is owed.
    #[test]
    fn one_outsized_delivery_does_not_forget_the_account() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 40);
        ticks += CYCLE;
        assert_eq!(run(&mut c, &mut ticks, 1), None, "a missed cycle");
        // Two cycles of clock and two cycles of audio in one callback: it
        // repays nothing, and says nothing about the graph's cycle.
        ticks += 2 * CYCLE;
        assert_eq!(c.delivered(ticks, GRAPH, 683), None);
        assert!(
            without_repaying(&mut c, &mut ticks, GRAPH, 341).is_some(),
            "a burst forgave a real deficit"
        );
    }

    /// The graph can change its quantum under a running stream, which is no
    /// stream state change either. The clock advances by the new cycle while
    /// the callback still carries the old one, once; that slip is geometry,
    /// not audio the graph overwrote.
    #[test]
    fn a_grown_graph_quantum_is_not_lost_audio() {
        let mut c = Continuity::new(16_000);
        let mut ticks = 0;
        run(&mut c, &mut ticks, 40);
        // Twice the quantum: one cycle of clock without its audio, then
        // cycles twice as long.
        let long = 2 * CYCLE;
        ticks += long;
        assert_eq!(c.delivered(ticks, GRAPH, 341), None);
        for _ in 0..40 {
            ticks += long;
            assert_eq!(
                c.delivered(ticks, GRAPH, 683),
                None,
                "a quantum change is lost audio"
            );
        }
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
        assert_eq!(c.delivered(ticks, rate, 372), None);
        assert!(without_repaying(&mut c, &mut ticks, rate, 372).is_some());
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

    fn select_channels_s16(
        data: &[u8],
        in_channels: usize,
        selected: &[u8],
        out_channels: usize,
    ) -> Vec<u8> {
        downmix_s16(data, in_channels, &channel_buckets(selected, out_channels)).collect()
    }

    fn s16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn data_path(capacity: usize, mix: Mix) -> (DataPath, rtrb::Consumer<u8>, Arc<Shared>) {
        let (tx, rx) = rtrb::RingBuffer::new(capacity);
        let shared = Arc::new(Shared::default());
        (DataPath::new(tx, shared.clone(), mix, 16_000), rx, shared)
    }

    fn mono() -> Mix {
        Mix::new(None, 1, 1)
    }

    fn producer() -> (Producer, Arc<crate::ring::Ring>) {
        let fmt = CoreFormat::default();
        let ring = crate::ring::Ring::new(1 << 20, fmt);
        let (stats, _) = tokio::sync::watch::channel(crate::AudioStats::default());
        let (health, _) = tokio::sync::watch::channel(myna_core::CaptureHealth::Opening);
        (Producer::new(ring.clone(), stats, health, fmt, 2, 2), ring)
    }

    #[test]
    fn a_buffer_the_realtime_ring_cannot_hold_overflows_instead_of_splitting() {
        let (mut path, mut rx, shared) = data_path(8, mono());
        assert_eq!(path.deliver(&[1; 6]), 3);
        assert_eq!(shared.loss(), None);
        assert_eq!(path.deliver(&[2; 4]), 2);
        assert_eq!(shared.loss(), Some(Loss::Overflow));
        // Nothing after a loss is handed over, even what would fit.
        assert_eq!(path.deliver(&[3; 2]), 1);
        let held: Vec<u8> = rx.read_chunk(rx.slots()).unwrap().into_iter().collect();
        assert_eq!(held, [1; 6]);
    }

    #[test]
    fn missed_cycles_latch_a_loss_and_the_first_loss_wins() {
        let (mut path, _rx, shared) = data_path(8, mono());
        path.account(0, GRAPH, 341);
        path.account(CYCLE, GRAPH, 341);
        assert_eq!(shared.loss(), None);
        path.account(3 * CYCLE, GRAPH, 341);
        assert_eq!(shared.loss(), None, "not before later audio leaves it owed");
        path.account(3 * CYCLE + 9_600, GRAPH, 3_200);
        let first = shared.loss();
        match first {
            Some(Loss::Missed(lost)) => {
                assert!(
                    lost > LOSS_TOLERANCE && lost < Duration::from_millis(22),
                    "{lost:?}"
                )
            }
            other => panic!("expected missed audio, got {other:?}"),
        }
        path.deliver(&[0; 16]);
        assert_eq!(shared.loss(), first);
    }

    /// Nothing is latched while a deficit is still a candidate, but the loop
    /// thread can read it when the capture ends before the confirmation does.
    #[test]
    fn an_unconfirmed_deficit_reaches_the_loop_thread() {
        let (mut path, rx, shared) = data_path(8, mono());
        let drain = Drain {
            ring: rx,
            shared: shared.clone(),
        };
        path.account(0, GRAPH, 341);
        path.account(CYCLE, GRAPH, 341);
        assert_eq!(drain.unconfirmed(), None);
        // Half a second of graph cycles the data thread never saw.
        let missed = CYCLE + 24_000;
        path.account(missed, GRAPH, 341);
        assert_eq!(shared.loss(), None, "not confirmed, so not latched");
        match drain.unconfirmed() {
            Some(Loss::Missed(lost)) => assert!(lost > Duration::from_millis(470), "{lost:?}"),
            other => panic!("expected the tail deficit, got {other:?}"),
        }
        // Repaid after all: the clock was ahead, not the audio gone.
        path.account(missed + CYCLE, GRAPH, 8_341);
        assert_eq!(drain.unconfirmed(), None);
    }

    #[test]
    fn a_state_change_restarts_the_loss_account() {
        let (mut path, _rx, shared) = data_path(8, mono());
        path.account(0, GRAPH, 341);
        shared.epoch.fetch_add(1, Ordering::AcqRel);
        let resumed = 48_000 * 10;
        path.account(resumed, GRAPH, 341);
        assert_eq!(shared.loss(), None);
        path.account(resumed + 2 * CYCLE, GRAPH, 341);
        path.account(resumed + 2 * CYCLE + 9_600, GRAPH, 3_200);
        assert!(shared.loss().is_some());
    }

    #[test]
    fn selected_channels_are_downmixed_into_the_realtime_ring() {
        let (mut path, mut rx, _) = data_path(64, Mix::new(Some(&[2, 3]), 4, 1));
        let frames = s16(&[100, 200, 300, 400, -100, -200, -300, -400]);
        assert_eq!(path.deliver(&frames), 2);
        let out: Vec<u8> = rx.read_chunk(rx.slots()).unwrap().into_iter().collect();
        assert_eq!(out, s16(&[350, -350]));
    }

    #[test]
    fn a_downmix_the_realtime_ring_cannot_hold_overflows() {
        let (mut path, rx, shared) = data_path(2, Mix::new(Some(&[0, 1]), 2, 1));
        assert_eq!(path.deliver(&s16(&[1, 3, 5, 7])), 2);
        assert_eq!(shared.loss(), Some(Loss::Overflow));
        assert_eq!(rx.slots(), 0);
    }

    #[test]
    fn a_mix_without_a_usable_selection_passes_the_stream_through() {
        for mix in [
            Mix::new(Some(&[]), 2, 2),
            Mix::new(Some(&[1]), 2, 0),
            Mix::new(None, 2, 2),
        ] {
            assert!(matches!(mix, Mix::Passthrough { frame_bytes: 4 }));
        }
        assert_eq!(Mix::new(None, 2, 2).frames(&[0; 9]), 2);
        assert_eq!(Mix::new(Some(&[0]), 3, 1).frames(&[0; 13]), 2);
        assert_eq!(Mix::new(Some(&[0]), 4, 1).frames(&[0; 24]), 3);
    }

    #[test]
    fn the_stream_is_wide_enough_for_every_selected_channel() {
        assert_eq!(stream_channels(Some(&[2, 3]), 1), 4);
        assert_eq!(stream_channels(Some(&[5, 0]), 2), 6);
        assert_eq!(stream_channels(Some(&[]), 2), 1);
        assert_eq!(stream_channels(None, 2), 2);
    }

    #[test]
    fn selected_channels_downmix_to_each_output_channel() {
        let (mut path, mut rx, _) = data_path(64, Mix::new(Some(&[1, 0]), 2, 2));
        assert_eq!(path.deliver(&s16(&[1, 2, 3, 4, 5, 6])), 3);
        let out: Vec<u8> = rx.read_chunk(rx.slots()).unwrap().into_iter().collect();
        assert_eq!(out, s16(&[2, 1, 4, 3, 6, 5]));
    }

    #[test]
    fn a_loss_outranks_a_clean_end_but_not_an_earlier_fault() {
        let lost = Loss::Missed(Duration::from_millis(40));
        assert_eq!(final_ending(Some(Ending::Clean), None), Some(Ending::Clean));
        assert_eq!(final_ending(None, None), None);
        assert_eq!(
            final_ending(Some(Ending::Clean), Some(lost)),
            Some(Ending::Fault(lost.into_error()))
        );
        assert_eq!(
            final_ending(None, Some(Loss::Overflow)),
            Some(Ending::Fault(Loss::Overflow.into_error()))
        );
        let earlier = Ending::Fault(CaptureError::DeviceUnavailable("gone".into()));
        assert_eq!(
            final_ending(
                Some(Ending::Fault(CaptureError::DeviceUnavailable(
                    "gone".into()
                ))),
                Some(lost)
            ),
            Some(earlier)
        );
    }

    #[tokio::test]
    async fn a_drain_moves_the_audio_buffered_before_a_loss_then_reports_it() {
        let (mut path, rx, shared) = data_path(8, mono());
        let mut drain = Drain { ring: rx, shared };
        let (mut producer, ring) = producer();
        path.deliver(&[1; 6]);
        assert_eq!(
            drain.run(&mut producer),
            Drained {
                delivered: true,
                abandoned: false,
                loss: None
            }
        );
        // Wraps the ring's end.
        path.deliver(&[2; 6]);
        path.deliver(&[3; 4]);
        assert_eq!(
            drain.run(&mut producer),
            Drained {
                delivered: true,
                abandoned: false,
                loss: Some(Loss::Overflow)
            }
        );
        assert!(!drain.run(&mut producer).delivered);
        producer.finish(None);
        let mut got = Vec::new();
        while let Some(Ok(chunk)) = ring.next().await {
            got.extend_from_slice(&chunk.data);
        }
        assert_eq!(got, [[1; 6], [2; 6]].concat());
    }

    #[test]
    fn a_drain_reports_a_consumer_that_has_gone() {
        let (mut path, rx, shared) = data_path(8, mono());
        let mut drain = Drain { ring: rx, shared };
        let (mut producer, ring) = producer();
        ring.close();
        assert!(!drain.run(&mut producer).abandoned);
        path.deliver(&[0; 2]);
        assert!(drain.run(&mut producer).abandoned);
    }

    #[test]
    fn the_realtime_ring_holds_its_buffer_of_the_negotiated_format() {
        let stereo = CoreFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            sample_width_bytes: 2,
        };
        let (tx, rx) = realtime_ring(stereo);
        assert_eq!(tx.slots(), 384_000);
        assert_eq!(rx.slots(), 0);
    }

    #[test]
    fn losses_are_backend_faults_naming_the_cause() {
        let shared = Shared::default();
        shared.latch(Loss::Missed(Duration::ZERO));
        assert_eq!(shared.loss(), Some(Loss::Missed(Duration::from_micros(1))));
        shared.latch(Loss::Missed(Duration::MAX));
        assert_eq!(
            shared.loss(),
            Some(Loss::Missed(Duration::from_micros(u64::MAX - 1)))
        );
        match Loss::Missed(Duration::from_millis(250)).into_error() {
            CaptureError::Backend(msg) => {
                assert!(msg.starts_with("250 ms of audio was lost"), "{msg}")
            }
            other => panic!("{other:?}"),
        }
        match Loss::Overflow.into_error() {
            CaptureError::Backend(msg) => {
                assert!(msg.contains("overflowed") && msg.contains("2 s"), "{msg}")
            }
            other => panic!("{other:?}"),
        }
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
        assert_eq!(select_channels_s16(&frame, 2, &[2], 1), s16(&[0]));
    }
}
