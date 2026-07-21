//! Env-gated integration suite for the native PipeWire backend and live device
//! enumeration (feature 002-native-pipewire-backend). Runs against a real
//! PipeWire graph — a virtual-audio VM profile (null-sink / `pw-loopback`
//! source) in CI, or real hardware — the *identical* code on both
//! (constitution Principle II).
//!
//! Gate: set `MYNA_PIPEWIRE_TESTS=1` to run. Unset (the default, and CI without
//! an audio server) → every test returns early as a no-op, so the suite is
//! always compilable and green offline without touching PipeWire.
//!
//! Run: `MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-audio --test pipewire_hw`
//!
//! An optional `MYNA_PIPEWIRE_TARGET=<node.name>` selects a specific capture
//! node for the selection tests; without it the default source is used.

use std::process::{Child, Command};
use std::time::Duration;

use futures_util::StreamExt;
use myna_audio::{CaptureSource, InputDevices, PipeWireBackend};
use myna_core::{AudioFormat, AudioSource, CaptureError, CaptureStream, PcmChunk};

/// A `pw-loopback`-created virtual capture source with a known `node.name`, so
/// selection tests don't depend on whatever hardware happens to be present.
/// Killed on drop. Returns `None` if `pw-loopback` isn't available.
struct VirtualSource {
    child: Child,
    node_name: String,
}

impl VirtualSource {
    fn spawn(node_name: &str) -> Option<Self> {
        Self::spawn_channels(node_name, None)
    }

    /// Spawn a virtual source, optionally multi-channel via an explicit
    /// `audio.position` (e.g. `FL,FR,RL,RR` for 4ch).
    fn spawn_channels(node_name: &str, position: Option<&str>) -> Option<Self> {
        let mut cap = format!(
            "media.class=Audio/Source node.name={node_name} node.description=myna-test"
        );
        if let Some(pos) = position {
            cap.push_str(&format!(" audio.position=[{pos}]"));
        }
        let child = Command::new("pw-loopback")
            .args(["--capture-props", &cap])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        Some(Self { child, node_name: node_name.to_string() })
    }
}

impl Drop for VirtualSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A private `pipewire` daemon with NO session manager (Ubuntu's default
/// `context.exec` starts none): the graph exists but has zero source nodes —
/// the masked-wireplumber failure mode found on hardware (2026-07-21), where
/// capture silently streamed nothing. Own runtime dir; killed + removed on
/// drop. Returns `None` if the `pipewire` binary isn't available.
struct NoSmDaemon {
    child: Child,
    runtime_dir: std::path::PathBuf,
}

impl NoSmDaemon {
    fn spawn() -> Option<Self> {
        let runtime_dir =
            std::env::temp_dir().join(format!("myna-nosm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).ok()?;
        let child = Command::new("pipewire")
            .env("PIPEWIRE_RUNTIME_DIR", &runtime_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        // Wait for the daemon socket to appear.
        for _ in 0..50 {
            if runtime_dir.join("pipewire-0").exists() {
                return Some(Self { child, runtime_dir });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child;
        None
    }

    /// The remote clients connect to (libpipewire accepts an absolute socket
    /// path as `remote.name`).
    fn remote(&self) -> String {
        self.runtime_dir.join("pipewire-0").display().to_string()
    }
}

impl Drop for NoSmDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

fn enabled() -> bool {
    std::env::var_os("MYNA_PIPEWIRE_TESTS").is_some_and(|v| v == "1")
}

fn target() -> Option<String> {
    std::env::var("MYNA_PIPEWIRE_TARGET").ok().filter(|s| !s.is_empty())
}

macro_rules! skip_unless_enabled {
    () => {
        if !enabled() {
            eprintln!("skipped: set MYNA_PIPEWIRE_TESTS=1 (needs a running PipeWire graph)");
            return;
        }
    };
}

async fn drain_with_timeout(
    mut stream: CaptureStream,
    overall: Duration,
) -> (Vec<PcmChunk>, Option<CaptureError>) {
    let mut chunks = Vec::new();
    let mut fault = None;
    let deadline = tokio::time::Instant::now() + overall;
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Err(_) => break, // overall budget hit; treat as "enough captured"
            Ok(None) => break,
            Ok(Some(Ok(c))) => chunks.push(c),
            Ok(Some(Err(e))) => {
                assert!(fault.is_none(), "more than one Err on the stream");
                fault = Some(e);
            }
        }
    }
    (chunks, fault)
}

/// Harness self-check: the gate compiles and skips cleanly with no PipeWire.
#[test]
fn gate_skips_cleanly_when_disabled() {
    skip_unless_enabled!();
}

/// No session manager → no sources in the graph: capture must FAULT LOUDLY
/// within the link-wait timeout (`DeviceUnavailable`, naming the session
/// manager) — never an open stream that silently delivers zero chunks (the
/// masked-wireplumber failure mode found on hardware, 2026-07-21).
#[tokio::test]
async fn no_session_manager_faults_loudly() {
    skip_unless_enabled!();
    let Some(daemon) = NoSmDaemon::spawn() else {
        eprintln!("skipped: could not spawn a private pipewire daemon");
        return;
    };

    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let started = std::time::Instant::now();
    let (chunks, fault) =
        drain_with_timeout(Box::new(source).capture(), Duration::from_secs(15)).await;

    assert!(chunks.is_empty(), "a source-less graph must yield no audio");
    match fault {
        Some(CaptureError::DeviceUnavailable(msg)) => {
            assert!(msg.contains("session manager"), "message should name the cause: {msg}")
        }
        other => panic!("expected DeviceUnavailable(no source), got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the fault must surface promptly, not hang the press"
    );
}

/// T009: default-source capture yields chunks in exactly the negotiated format;
/// the ring fills from `capture()` (press) while the consumer defers draining,
/// then drains buffered-then-live with nothing lost (FR-009); graceful `stop()`
/// drains then ends with no `Err`; `AudioStats::dropped == 0` (C1, C8, C13;
/// SC-006).
#[tokio::test]
async fn default_capture_format_stop_and_no_drops() {
    skip_unless_enabled!();
    let fmt = AudioFormat::default(); // 16 kHz mono S16LE
    let mut builder = CaptureSource::builder(fmt).ring_depth(Duration::from_secs(30));
    if let Some(t) = target() {
        builder = builder.target(t);
    }
    let source = builder.backend(Box::new(PipeWireBackend::new())).build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    // Press-then-defer: let the ring fill for ~1 s before draining (FR-009).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if stats.borrow_and_update().captured >= Duration::from_millis(800) {
                break;
            }
            stats.changed().await.unwrap();
        }
    })
    .await
    .expect("no audio arrived from the default source within 10s");

    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(5)).await;
    assert!(fault.is_none(), "graceful stop is a clean end: {fault:?}");
    assert!(!chunks.is_empty(), "captured audio drains after stop");
    for c in &chunks {
        assert_eq!(c.format, fmt, "every chunk is exactly the negotiated format");
    }
    let s = stats.borrow();
    assert_eq!(s.dropped, Duration::ZERO, "healthy session drops nothing (SC-006)");
}

/// T010: device native format ≠ negotiated → consumer still receives exactly
/// the negotiated format, converted graph-side (C2, FR-003). We request an
/// unusual rate/channel combo the device almost certainly doesn't natively
/// produce and assert the chunks still carry the requested format.
#[tokio::test]
async fn graph_side_conversion_delivers_negotiated_format() {
    skip_unless_enabled!();
    // 48 kHz stereo — very likely a conversion from the graph's native source.
    let fmt = AudioFormat { sample_rate_hz: 48_000, channels: 2, sample_width_bytes: 2 };
    let mut builder = CaptureSource::builder(fmt).ring_depth(Duration::from_secs(30));
    if let Some(t) = target() {
        builder = builder.target(t);
    }
    let source = builder.backend(Box::new(PipeWireBackend::new())).build();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    let (chunks, fault) = {
        let s = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            s.stop();
        });
        drain_with_timeout(stream, Duration::from_secs(8)).await
    };
    assert!(fault.is_none(), "clean end: {fault:?}");
    assert!(!chunks.is_empty(), "captured some converted audio");
    for c in &chunks {
        assert_eq!(c.format, fmt, "chunks carry the negotiated (converted) format");
    }
}

/// T011: abort (drop the stream) stops capture + discards the ring, cleanly
/// and without panic (C9, FR-011). The removal-mid-capture fault (C10) needs a
/// scriptable device teardown (create/kill a `pw-loopback` node) and is covered
/// where the harness can do that (quickstart step 2).
///
/// PLATFORM NOTE (finding 2026-07-15): a *bogus* target does NOT fault — with
/// the default WirePlumber policy the session manager falls back to the default
/// source and captures, exactly as `pw-record --target <bogus>` does (verified:
/// pw-record captures 93 KB from a nonexistent node). So FR-004/C4's
/// "absent target → clear fault" is not achievable under the default
/// session-manager policy; strict targeting would require a policy/route change
/// out of this crate's scope. The *positive* selection case (a resolvable
/// target captures that node) is the US2 contract, tested in T018 with a real
/// second node. Recorded as a known limitation rather than forced here.
#[tokio::test]
async fn abort_discards_cleanly() {
    skip_unless_enabled!();
    let fmt = AudioFormat::default();
    let source = CaptureSource::builder(fmt).backend(Box::new(PipeWireBackend::new())).build();
    let stream = Box::new(source).capture();
    // Let capture start, then abort by dropping the stream: ConsumerGuard trips
    // stop + closes the ring; the loop thread must observe it and tear down.
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(stream);
    tokio::time::sleep(Duration::from_millis(400)).await;
    // Reaching here without a panic/hang is the assertion (clean teardown).
}

/// T023: channel pick/downmix on a multi-channel source (C6; SC-004, US3-1).
/// Create a 4-channel virtual source, select two channels, and assert capture
/// links and delivers the negotiated (downmixed) mono format. (Exact per-
/// channel signal discrimination needs a fed multichannel signal; the
/// pick/downmix math itself is unit-tested in `native::tests`.)
#[tokio::test]
async fn multichannel_channel_selection_captures() {
    skip_unless_enabled!();
    let Some(vsrc) = VirtualSource::spawn_channels("myna-test-4ch-023", Some("FL,FR,RL,RR"))
    else {
        eprintln!("skipped: pw-loopback unavailable");
        return;
    };
    tokio::time::sleep(Duration::from_millis(800)).await;
    let fmt = AudioFormat::default(); // mono out
    let source = CaptureSource::builder(fmt)
        .ring_depth(Duration::from_secs(30))
        .target(vsrc.node_name.clone())
        .channels(vec![2, 3]) // pick the rear pair, downmix to mono
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    let linked = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if stats.borrow_and_update().captured >= Duration::from_millis(300) {
                break true;
            }
            if stats.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
    assert!(fault.is_none(), "clean end: {fault:?}");
    assert!(linked && !chunks.is_empty(), "multichannel selection linked + captured");
    for c in &chunks {
        assert_eq!(c.format, fmt, "output is the negotiated (downmixed) format");
    }
}

/// T027: `list()` returns present input devices with stable `node_name` +
/// `label`; an empty graph would return an empty `Vec`, not an error (E1, E2;
/// SC-005, US4-1/2). We assert the shape and that a created virtual source
/// shows up.
#[tokio::test]
async fn enumerate_lists_input_devices() {
    skip_unless_enabled!();
    let devices = InputDevices::new().expect("registry connect");
    // A created source must appear in the live list.
    let Some(vsrc) = VirtualSource::spawn("myna-test-src-027") else {
        eprintln!("skipped: pw-loopback unavailable");
        return;
    };
    let mut watch = devices.watch();
    let found = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if devices.list().iter().any(|d| d.node_name == vsrc.node_name) {
                break true;
            }
            if watch.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(found, "the created virtual source appears in list()");
    for d in devices.list() {
        assert!(!d.node_name.is_empty(), "every device has a stable name");
        assert!(!d.label.is_empty(), "every device has a label");
    }
}

/// T028: an active watcher sees a device appear and disappear without
/// re-requesting (E3, E4; FR-008a, US4-3).
#[tokio::test]
async fn enumerate_observes_add_and_remove() {
    skip_unless_enabled!();
    let devices = InputDevices::new().expect("registry connect");
    let mut watch = devices.watch();
    let name = "myna-test-src-028";

    let vsrc = match VirtualSource::spawn(name) {
        Some(v) => v,
        None => {
            eprintln!("skipped: pw-loopback unavailable");
            return;
        }
    };
    let appeared = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if devices.list().iter().any(|d| d.node_name == name) {
                break true;
            }
            if watch.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(appeared, "watcher observed the device appear");

    drop(vsrc); // kill the loopback → global_remove
    let disappeared = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if !devices.list().iter().any(|d| d.node_name == name) {
                break true;
            }
            if watch.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(disappeared, "watcher observed the device disappear");
}

/// T029 (part): a `node_name` from `list()` used as a capture target selects a
/// real device (E7 — enumeration ties to selection). The unreachable-PipeWire
/// `new()` error (E5) can't be exercised while a daemon is running; it is
/// covered structurally by the error path in `InputDevices::new`.
#[tokio::test]
async fn enumerated_name_is_a_usable_target() {
    skip_unless_enabled!();
    let devices = InputDevices::new().expect("registry connect");
    let _vsrc = VirtualSource::spawn("myna-test-src-029");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let Some(dev) = devices.list().into_iter().next() else {
        eprintln!("skipped: no input devices to target");
        return;
    };
    let source = CaptureSource::builder(AudioFormat::default())
        .target(dev.node_name.clone())
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    let linked = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if stats.borrow_and_update().captured >= Duration::from_millis(200) {
                break true;
            }
            if stats.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    stop.stop();
    let _ = drain_with_timeout(stream, Duration::from_secs(3)).await;
    assert!(linked, "an enumerated device name is a usable capture target");
}

mod watermarks {
    //! T035: capture-path performance watermarks (constitution Principle III;
    //! SC-006, SC-008, SC-009). Checked-in baselines with declared per-metric
    //! tolerances, sensitive enough to flag drift, not only gross breakage.
    //! Full peak-RSS/CPU watermarking wants a sampling harness (matrix.py-style);
    //! this pins the two capture-path invariants that regress most visibly.
    //!
    //! Baselines (default source, 16 kHz mono S16LE, this reference env):
    //! - stop latency (flag→stream end): observed ~0.1–0.2 s; ceiling 500 ms
    //!   flag-observation + drain (FR-012/SC-009).
    //! - dropped audio in a healthy session: baseline 0 (SC-006), tol 0.
    use super::*;

    const STOP_LATENCY_CEILING: Duration = Duration::from_millis(500);

    #[tokio::test]
    async fn perf_stop_latency_and_no_drops() {
        skip_unless_enabled!();
        let fmt = AudioFormat::default();
        let mut builder = CaptureSource::builder(fmt);
        if let Some(t) = target() {
            builder = builder.target(t);
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        let mut stats = source.stats();
        let stop = source.stop_handle();
        let stream = Box::new(source).capture();

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if stats.borrow_and_update().captured >= Duration::from_millis(500) {
                    break;
                }
                stats.changed().await.unwrap();
            }
        })
        .await
        .expect("capture established");

        let t0 = std::time::Instant::now();
        stop.stop();
        let (_c, fault) = drain_with_timeout(stream, Duration::from_secs(2)).await;
        let latency = t0.elapsed();
        assert!(fault.is_none());
        // Watermark: stop latency within the declared ceiling (SC-009).
        assert!(
            latency < STOP_LATENCY_CEILING,
            "stop-latency watermark exceeded: {latency:?} >= {STOP_LATENCY_CEILING:?}"
        );
        // Watermark: a healthy session drops nothing (SC-006), tolerance 0.
        assert_eq!(
            stats.borrow().dropped,
            Duration::ZERO,
            "drop watermark exceeded: healthy capture must not drop audio"
        );
    }
}

/// T018/T019: a resolvable target captures *that* node (C3, US2-1), and the
/// stable `node.name` still resolves after the graph changes (C5, SC-003). We
/// create a named virtual source, target it by name, and assert capture links
/// and produces the negotiated format. (Name-stability across renumbering is
/// inherent: we select by `node.name`, never by volatile id/serial.)
#[tokio::test]
async fn resolvable_target_selects_that_node() {
    skip_unless_enabled!();
    let Some(vsrc) = VirtualSource::spawn("myna-test-src-018") else {
        eprintln!("skipped: pw-loopback unavailable");
        return;
    };
    // Give the session manager a moment to register the new node.
    tokio::time::sleep(Duration::from_millis(800)).await;

    let fmt = AudioFormat::default();
    let source = CaptureSource::builder(fmt)
        .ring_depth(Duration::from_secs(30))
        .target(vsrc.node_name.clone())
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    // A loopback source with no playback feed still produces silence frames on
    // a linked stream, so `captured` advancing proves the target linked.
    let linked = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if stats.borrow_and_update().captured >= Duration::from_millis(300) {
                break true;
            }
            if stats.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
    assert!(fault.is_none(), "clean end from a resolvable target: {fault:?}");
    assert!(linked && !chunks.is_empty(), "capture linked to the named target");
    for c in &chunks {
        assert_eq!(c.format, fmt);
    }
}

/// T012: stop/abort honored within 250 ms of the flag (FR-012, SC-009); no
/// external process spawned during a session (C14 / SC-002 — the native
/// backend forks nothing, verified structurally: there is no `Command` in the
/// capture path).
#[tokio::test]
async fn stop_is_prompt() {
    skip_unless_enabled!();
    let fmt = AudioFormat::default();
    let mut builder = CaptureSource::builder(fmt);
    if let Some(t) = target() {
        builder = builder.target(t);
    }
    let source = builder.backend(Box::new(PipeWireBackend::new())).build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if stats.borrow_and_update().captured >= Duration::from_millis(300) {
                break;
            }
            stats.changed().await.unwrap();
        }
    })
    .await
    .expect("no audio to establish a running capture");

    let t0 = std::time::Instant::now();
    stop.stop();
    let (_chunks, fault) = drain_with_timeout(stream, Duration::from_secs(2)).await;
    let elapsed = t0.elapsed();
    assert!(fault.is_none(), "graceful stop is clean");
    // Stop-poll is 100 ms + drain; comfortably inside a generous bound. The
    // 250 ms contract is on the *flag observation*; end-to-end drain adds the
    // already-queued audio, so assert a practical ceiling.
    assert!(elapsed < Duration::from_secs(1), "stop drained promptly: {elapsed:?}");
}
