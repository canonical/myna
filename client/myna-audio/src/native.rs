//! [`PipeWireBackend`] (plan T52) — native live capture via `pipewire-rs`,
//! behind the [`CaptureBackend`] seam. No subprocess: a dedicated PipeWire
//! main-loop thread owns a capture `Stream`, and its `process` callback pushes
//! PCM straight into the adapter's ring via [`Producer::push`] (which never
//! blocks — overflow is the ring's drop-oldest problem).
//!
//! Replaces the `pw-record` subprocess backend (feature
//! 002-native-pipewire-backend, FR-016). Adds what the subprocess couldn't do
//! in-process: node selection by stable `node.name` (T021), channel
//! pick/downmix on multi-channel interfaces (T025), and graph-side
//! resample/downmix to the negotiated format (audio-adapter-api §7/§9). Real
//! DSP stays in the PipeWire graph upstream of our node (§10) — this backend
//! only selects, converts, observes.
//!
//! The module is named `native` (not `pipewire`) to avoid colliding with the
//! `pipewire` crate.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

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

/// How often the loop wakes to check the [`StopHandle`], so a graceful stop /
/// abort is honored within the ~250 ms promptness contract (audio-adapter-api
/// §5, FR-012) even when no audio is flowing.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Dead-capture watchdog window (2026-07-21 hardware finding): a daemon with
/// no session manager accepts `stream.connect` and even negotiates the stream
/// to `Paused` — but nothing ever links it to a source node, so `process`
/// never fires and the capture would masquerade as a clean, silent, empty
/// stream (§3). Two guards: at open, a registry roundtrip requires a usable
/// source to exist ([`no_source_message`]); after open, a stream that has
/// produced zero buffers within this window is declared dead
/// ([`no_flow_message`]). Healthy graphs deliver buffers within milliseconds
/// of streaming; 3 s is generous headroom.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(3);

/// The open-time link-wait decision for one stream-state transition.
enum LinkWait {
    /// Not wired yet — keep waiting (bounded by [`LINK_WAIT_TIMEOUT`]).
    Waiting,
    /// The graph negotiated the stream: a source is wired; open succeeds.
    Wired,
    /// The stream errored before wiring — fault the open.
    Errored,
}

/// Pure decision: which stream states prove a source was wired.
fn link_wait_on_state(new: &StreamState) -> LinkWait {
    match new {
        StreamState::Paused | StreamState::Streaming => LinkWait::Wired,
        StreamState::Error(_) => LinkWait::Errored,
        _ => LinkWait::Waiting,
    }
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
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer) -> Result<(), CaptureError> {
        // Only S16LE lives in the format universe today (audio-adapter-api §2,
        // pending T33). Reject other widths up front — cheap, testable offline
        // (T008), no PipeWire connection needed.
        if spec.format.sample_width_bytes != 2 {
            return Err(CaptureError::UnsupportedFormat(spec.format));
        }
        // Validate channel-index selection up front (T026): indices must be
        // non-empty and downmix to the negotiated channel count. The actual
        // pick/downmix happens graph-side + in the process callback (T025).
        if let Some(indices) = &spec.channels {
            if indices.is_empty() {
                return Err(CaptureError::Backend(
                    "channel selection is empty; give at least one channel index".into(),
                ));
            }
        }

        // Hand off to the loop thread; it reports open success/failure back
        // here synchronously so `start` returns the open error (§5).
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), CaptureError>>();
        spawn_capture_thread(spec, producer, ready_tx, self.remote);
        match ready_rx.recv() {
            Ok(result) => result,
            Err(_) => Err(CaptureError::Backend(
                "PipeWire capture thread exited before signaling readiness".into(),
            )),
        }
    }
}

/// Spawn the dedicated PipeWire main-loop thread (T005 + T013). The loop and
/// its objects are not `Send`, so everything PipeWire lives here. Reports
/// open success/failure through `ready_tx`; runtime faults go through
/// `producer.finish(Some(..))`; a clean stop/EOF through `finish(None)`.
fn spawn_capture_thread(
    spec: CaptureSpec,
    producer: Producer,
    ready_tx: mpsc::Sender<Result<(), CaptureError>>,
    remote: Option<String>,
) {
    std::thread::Builder::new()
        .name("myna-pw-capture".into())
        .spawn(move || {
            run_capture(&spec, producer, ready_tx.clone(), remote.as_deref());
            // Backstop: if run_capture returned before signaling (it always
            // signals on every path), make sure `start` can never block.
            let _ = ready_tx.send(Ok(()));
        })
        .expect("spawning the PipeWire capture thread");
}

/// The capture body: build loop + stream, connect, then run until stop/abort/
/// fault. Owns the [`Producer`] and calls `finish` exactly once (only after a
/// successful open). `ready_tx` carries the open outcome to `start`; on an
/// open failure the error is surfaced through `start`'s return (the producer
/// is simply dropped, exactly as `source.rs` expects on the `Err` path).
fn run_capture(
    spec: &CaptureSpec,
    producer: Producer,
    ready_tx: mpsc::Sender<Result<(), CaptureError>>,
    remote: Option<&str>,
) {
    // The open outcome is signalled exactly once, by whichever path comes
    // first: an early return below, the link-wait succeeding (state reaches
    // Paused/Streaming), the link-wait timeout, or the loop-quit catch-all.
    let ready_tx = Rc::new(RefCell::new(Some(ready_tx)));
    macro_rules! fail_open {
        ($err:expr) => {{
            if let Some(tx) = ready_tx.borrow_mut().take() {
                let _ = tx.send(Err($err));
            }
            return;
        }};
    }

    let main_loop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            fail_open!(CaptureError::DeviceUnavailable(format!(
                "cannot create PipeWire loop: {e}"
            )));
        }
    };
    let context = match ContextRc::new(&main_loop, None) {
        Ok(c) => c,
        Err(e) => {
            fail_open!(CaptureError::DeviceUnavailable(format!(
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
            fail_open!(CaptureError::DeviceUnavailable(format!(
                "cannot connect to PipeWire: {e}"
            )));
        }
    };

    // No-source graph check (see SILENCE_TIMEOUT): refuse to open a stream
    // the graph could never feed. Skipped roundtrips would be a silent empty
    // capture — the exact failure this guards (§3).
    match graph_has_source(&main_loop, &core, spec.target.as_deref()) {
        Ok(true) => {}
        Ok(false) => {
            fail_open!(CaptureError::DeviceUnavailable(no_source_message(
                spec.target.as_deref()
            )));
        }
        Err(e) => fail_open!(e),
    }

    // Producer + terminal fault shared with the loop callbacks (single thread,
    // so Rc<RefCell<..>> is sound and never contended).
    let producer = Rc::new(RefCell::new(Some(producer)));
    let fault: Rc<RefCell<Option<CaptureError>>> = Rc::new(RefCell::new(None));

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
            fail_open!(CaptureError::Backend(format!(
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

    // Open negotiation + the dead-stream watchdog (see SILENCE_TIMEOUT):
    // reaching Paused/Streaming completes open; buffers flowing prove life.
    let opened = Rc::new(Cell::new(false));
    let buffers_seen = Rc::new(Cell::new(0u64));

    let stop = spec.stop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let main_loop = main_loop.clone();
            let fault = fault.clone();
            let target = spec.target.clone();
            let opened = opened.clone();
            let ready_tx = ready_tx.clone();
            move |_stream, _ud, old, new| {
                if !opened.get() && matches!(link_wait_on_state(&new), LinkWait::Wired) {
                    opened.set(true);
                    if let Some(tx) = ready_tx.borrow_mut().take() {
                        let _ = tx.send(Ok(()));
                    }
                }
                if let StreamState::Error(msg) = &new {
                    // A stream error mid-capture (e.g. the device/daemon went
                    // away) → one terminal fault, then quit (FR-010, C10).
                    let detail = match &target {
                        Some(t) => format!("PipeWire stream error on '{t}': {msg}"),
                        None => format!("PipeWire stream error: {msg}"),
                    };
                    *fault.borrow_mut() = Some(CaptureError::DeviceUnavailable(detail));
                    main_loop.quit();
                }
                let _ = old;
            }
        })
        .process({
            let main_loop = main_loop.clone();
            let producer = producer.clone();
            let stop = stop.clone();
            let buffers_seen = buffers_seen.clone();
            // Channel pick/downmix config (§9): pick these input-channel indices
            // from the `stream_channels`-wide stream and average them down to
            // `out_channels`. `None` = pass through unchanged.
            let selection = selection.clone();
            let in_channels = stream_channels as usize;
            let out_channels = spec.format.channels as usize;
            move |stream, _ud| {
                while let Some(mut buffer) = stream.dequeue_buffer() {
                    buffers_seen.set(buffers_seen.get() + 1);
                    let datas = buffer.datas_mut();
                    if let Some(data) = datas.first_mut() {
                        let size = data.chunk().size() as usize;
                        let offset = data.chunk().offset() as usize;
                        if let Some(samples) = data.data() {
                            let end = (offset + size).min(samples.len());
                            let slice = &samples[offset.min(samples.len())..end];
                            if !slice.is_empty() {
                                let bytes = match &selection {
                                    Some(idx) => {
                                        select_channels_s16(slice, in_channels, idx, out_channels)
                                    }
                                    None => Bytes::copy_from_slice(slice),
                                };
                                let alive = producer
                                    .borrow_mut()
                                    .as_mut()
                                    .map(|p| p.push(bytes))
                                    .unwrap_or(false);
                                if !alive || stop.is_stopped() {
                                    // Consumer gone (abort) or stop tripped →
                                    // end promptly (FR-011).
                                    main_loop.quit();
                                }
                            }
                        }
                    }
                }
            }
        })
        .register();
    let _listener = match _listener {
        Ok(l) => l,
        Err(e) => {
            fail_open!(CaptureError::Backend(format!(
                "cannot register stream listener: {e}"
            )));
        }
    };

    // Stop-poll timer: quit within STOP_POLL of a graceful stop / abort even
    // when no audio is flowing (FR-012, SC-009).
    let timer = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        let stop = stop.clone();
        move |_| {
            if stop.is_stopped() {
                main_loop.quit();
            }
        }
    });
    let _ = timer
        .update_timer(Some(STOP_POLL), Some(STOP_POLL))
        .into_result();

    // Dead-stream watchdog (one-shot): an opened stream that produced zero
    // buffers within the window is dead — fault loudly (as an open failure if
    // negotiation somehow never completed, else a terminal runtime fault).
    let watchdog = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        let ready_tx = ready_tx.clone();
        let fault = fault.clone();
        let target = spec.target.clone();
        let buffers_seen = buffers_seen.clone();
        move |_| {
            if buffers_seen.get() > 0 {
                return;
            }
            let msg = no_flow_message(target.as_deref());
            *fault.borrow_mut() = Some(CaptureError::DeviceUnavailable(msg.clone()));
            if let Some(tx) = ready_tx.borrow_mut().take() {
                let _ = tx.send(Err(CaptureError::DeviceUnavailable(msg)));
            }
            main_loop.quit();
        }
    });
    let _ = watchdog
        .update_timer(Some(SILENCE_TIMEOUT), None)
        .into_result();

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

    if let Err(e) = stream.connect(Direction::Input, None, flags, &mut params) {
        let err = match &spec.target {
            Some(t) => CaptureError::DeviceUnavailable(format!("cannot connect to '{t}': {e}")),
            None => CaptureError::Backend(format!("cannot connect capture stream: {e}")),
        };
        fail_open!(err);
    }

    // Open success is signalled from `state_changed` once the stream is
    // wired (Paused/Streaming), or the link-wait timer faults it — the loop
    // must run for either, so `start` stays parked until then.
    main_loop.run();

    // Loop quit: stop/abort/fault/link-timeout. Deliver exactly one terminal
    // outcome to the consumer stream (queued audio drains first, then this).
    drop(timer);
    drop(watchdog);
    let was_opened = opened.get();
    if !was_opened {
        // Stop/abort during the link wait: close out the unsignalled open as
        // a failure — never an empty stream masquerading as a clean end (§3).
        if let Some(tx) = ready_tx.borrow_mut().take() {
            let _ = tx.send(Err(CaptureError::DeviceUnavailable(
                "capture stopped before an audio source was wired".into(),
            )));
        }
    }
    let fault = fault.borrow_mut().take();
    let producer = producer.borrow_mut().take();
    if let Some(p) = producer {
        // On open failure `start` already returned Err and the ring was
        // dropped: drop the producer quietly, mirroring the other
        // open-failure paths. Only an opened stream finishes (fault or clean).
        if was_opened {
            p.finish(fault);
        }
    }
}

/// Pick channel indices `selected` from an interleaved S16LE frame stream that
/// has `in_channels` channels, and downmix them to `out_channels` by averaging
/// (audio-adapter-api §9). Frame-aligned; a trailing partial frame is dropped.
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

    /// No-source link wait (2026-07-21 hardware finding): a daemon with no
    /// session manager accepts `stream.connect` but never wires the stream to
    /// a node — it sits in Connecting forever. Only Paused/Streaming prove a
    /// source was wired; Error faults; everything else keeps waiting (bounded
    /// by LINK_WAIT_TIMEOUT).
    #[test]
    fn link_wait_decision() {
        assert!(matches!(
            link_wait_on_state(&StreamState::Paused),
            LinkWait::Wired
        ));
        assert!(matches!(
            link_wait_on_state(&StreamState::Streaming),
            LinkWait::Wired
        ));
        assert!(matches!(
            link_wait_on_state(&StreamState::Connecting),
            LinkWait::Waiting
        ));
        assert!(matches!(
            link_wait_on_state(&StreamState::Unconnected),
            LinkWait::Waiting
        ));
        assert!(matches!(
            link_wait_on_state(&StreamState::Error("no node".into())),
            LinkWait::Errored
        ));
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
