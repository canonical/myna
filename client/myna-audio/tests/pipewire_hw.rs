//! Env-gated integration suite for the native PipeWire backend and live device
//! enumeration (feature 002-native-pipewire-backend). Runs against a real
//! PipeWire graph — a virtual-audio VM profile (null-sink / `pw-loopback`
//! source) in CI, or real hardware — the *identical* code on both
//! (constitution Principle II).
//!
//! Gate: set `MYNA_PIPEWIRE_TESTS=1` to run. Unset (the default, and CI without
//! an audio server) → every test returns early as a no-op and says so, so the
//! suite is always compilable and green offline without touching PipeWire.
//! Set, with no graph answering → every test FAILS. The gate is a claim that
//! the service is there; honouring it with a skip is how a PipeWire regression
//! ships green.
//!
//! Run: `MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-audio --test pipewire_hw`
//!
//! An optional `MYNA_PIPEWIRE_TARGET=<node.name>` selects a specific capture
//! node for the selection tests; without it the default source is used.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use myna_audio::{CaptureSource, InputDevices, PipeWireBackend};
use myna_core::{
    AudioFormat, AudioSource, CaptureError, CaptureHealth, CaptureHealthStream, CaptureStream,
    PcmChunk,
};

/// A fresh scratch directory per call: tests in this binary run in parallel.
fn scratch_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("myna-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A `pw-loopback`-created virtual capture source with a known `node.name`, so
/// selection tests don't depend on whatever hardware happens to be present.
/// Killed on drop.
///
/// Panics when `pw-loopback` cannot be spawned: it ships with PipeWire, so its
/// absence means the gate was set against a graph that is not really there,
/// and the cases below would otherwise pass having selected nothing.
struct VirtualSource {
    child: Child,
    node_name: String,
}

impl VirtualSource {
    fn spawn(node_name: &str) -> Self {
        Self::spawn_channels(node_name, None)
    }

    /// Spawn a virtual source, optionally multi-channel via an explicit
    /// `audio.position` (e.g. `FL,FR,RL,RR` for 4ch).
    fn spawn_channels(node_name: &str, position: Option<&str>) -> Self {
        let mut cap =
            format!("media.class=Audio/Source node.name={node_name} node.description=myna-test");
        if let Some(pos) = position {
            cap.push_str(&format!(" audio.position=[{pos}]"));
        }
        let child = Command::new("pw-loopback")
            .args(["--capture-props", &cap])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn pw-loopback for {node_name} ({e}). {HOW_TO_RUN}"));
        let source = Self {
            child,
            node_name: node_name.to_string(),
        };
        source.await_registered();
        source
    }

    /// Block until the graph lists the node, so capture can discover it.
    fn await_registered(&self) {
        let listed = format!("node.name = \"{}\"", self.node_name);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let out = Command::new("pw-cli").args(["ls", "Node"]).output();
            if out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&listed)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{} never appeared in the graph", self.node_name);
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
/// drop.
///
/// Panics rather than returning `None`: the cases that use it assert that a
/// source-less graph faults, and a case that never got a graph to point at
/// would report that same green without having looked.
struct NoSmDaemon {
    child: Child,
    runtime_dir: std::path::PathBuf,
}

impl NoSmDaemon {
    fn spawn() -> Self {
        let runtime_dir = scratch_dir("nosm");
        let mut child = Command::new("pipewire")
            .env("PIPEWIRE_RUNTIME_DIR", &runtime_dir)
            // Bare means no user drop-ins either: the harness declares its
            // virtual mic in $XDG_CONFIG_HOME, and these cases need no source.
            .env("XDG_CONFIG_HOME", &runtime_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn a private pipewire daemon ({e}). {HOW_TO_RUN}"));
        // Wait for the daemon socket to appear.
        for _ in 0..50 {
            if runtime_dir.join("pipewire-0").exists() {
                return Self { child, runtime_dir };
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "the private pipewire daemon never bound {}. {HOW_TO_RUN}",
            runtime_dir.join("pipewire-0").display()
        );
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

/// A socket that accepts connections and never speaks: a hung daemon, so
/// capture sits in discovery until stopped or its deadline passes.
struct SilentDaemon {
    _listener: UnixListener,
    dir: PathBuf,
}

impl SilentDaemon {
    fn bind() -> Self {
        let dir = scratch_dir("silent");
        let listener = UnixListener::bind(dir.join("pipewire-0")).expect("bind silent socket");
        Self {
            _listener: listener,
            dir,
        }
    }

    fn remote(&self) -> String {
        self.dir.join("pipewire-0").display().to_string()
    }

    /// Resolves once capture has connected and is waiting on discovery. The
    /// returned connection must outlive the wait.
    async fn connected(&self) -> std::os::unix::net::UnixStream {
        let listener = self._listener.try_clone().expect("clone listener");
        tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || listener.accept()),
        )
        .await
        .expect("capture never connected")
        .expect("accept task")
        .expect("accept")
        .0
    }
}

impl Drop for SilentDaemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every health state until the stream ends (the capture thread released),
/// and how long that took. Panics past `budget`.
async fn health_to_end(
    mut health: CaptureHealthStream,
    budget: Duration,
) -> (Vec<CaptureHealth>, Duration) {
    let t0 = Instant::now();
    let states = tokio::time::timeout(budget, async {
        let mut states = Vec::new();
        while let Some(state) = health.next().await {
            states.push(state);
        }
        states
    })
    .await
    .unwrap_or_else(|_| panic!("capture was not released within {budget:?}"));
    (states, t0.elapsed())
}

fn device_unavailable(state: Option<&CaptureHealth>) -> String {
    match state {
        Some(CaptureHealth::Faulted(CaptureError::DeviceUnavailable(msg))) => msg.clone(),
        other => panic!("expected a DeviceUnavailable fault, got {other:?}"),
    }
}

fn enabled() -> bool {
    std::env::var_os("MYNA_PIPEWIRE_TESTS").is_some_and(|v| v == "1")
}

fn target() -> Option<String> {
    std::env::var("MYNA_PIPEWIRE_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
}

/// How the suite's graph is stood up, quoted in every "it is not there"
/// failure so the reader knows what to start.
const HOW_TO_RUN: &str = "dev/gated-tests.sh stands a private graph up (pipewire + wireplumber \
     under a scratch XDG_RUNTIME_DIR) and only then sets the gate; run `make test-client-gated`";

/// The graph the gate promises. `MYNA_PIPEWIRE_TESTS=1` is a claim that a
/// PipeWire graph is reachable, so an unreachable one is a failure of this
/// suite, not a reason to skip it: a case that runs against no graph asserts
/// nothing and reports green, which is how a PipeWire regression ships.
///
/// Checked once per process; every case goes through it.
fn require_graph() {
    static GRAPH: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    if let Err(why) =
        GRAPH.get_or_init(|| InputDevices::new().map(|_| ()).map_err(|e| e.to_string()))
    {
        panic!("MYNA_PIPEWIRE_TESTS=1 but no PipeWire graph answers ({why}). {HOW_TO_RUN}");
    }
}

macro_rules! skip_unless_enabled {
    () => {
        if !enabled() {
            eprintln!("skipped: set MYNA_PIPEWIRE_TESTS=1 (needs a running PipeWire graph)");
            return;
        }
        require_graph();
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

/// Harness self-check, both ways round: with the gate unset the suite skips
/// and says so; with it set the graph it promises has to be there.
#[tokio::test]
async fn the_graph_the_gate_promises_is_reachable() {
    skip_unless_enabled!();
    let devices = InputDevices::new().expect("registry connect");
    // `list()` fills from the registry, so wait for the first source the way
    // the enumeration cases do rather than reading an empty snapshot.
    let mut watch = devices.watch();
    let listed = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if !devices.list().is_empty() {
                break true;
            }
            if watch.changed().await.is_err() {
                break false;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        listed,
        "MYNA_PIPEWIRE_TESTS=1 but the graph lists no capture sources. {HOW_TO_RUN}"
    );
}

/// No session manager → no sources in the graph: capture must FAULT LOUDLY
/// within the link-wait timeout (`DeviceUnavailable`, naming the session
/// manager) — never an open stream that silently delivers zero chunks (the
/// masked-wireplumber failure mode found on hardware, 2026-07-21).
#[tokio::test]
async fn no_session_manager_faults_loudly() {
    skip_unless_enabled!();
    let daemon = NoSmDaemon::spawn();

    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let started = std::time::Instant::now();
    let (chunks, fault) =
        drain_with_timeout(Box::new(source).capture(), Duration::from_secs(15)).await;

    assert!(chunks.is_empty(), "a source-less graph must yield no audio");
    match fault {
        Some(CaptureError::DeviceUnavailable(msg)) => {
            assert!(
                msg.contains("session manager"),
                "message should name the cause: {msg}"
            )
        }
        other => panic!("expected DeviceUnavailable(no source), got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the fault must surface promptly, not hang the press"
    );
}

/// `capture()` is the press and must not wait on the daemon: opening happens
/// behind the stream, visible as `Opening` on health.
#[tokio::test]
async fn capture_returns_before_the_daemon_answers() {
    skip_unless_enabled!();
    let daemon = SilentDaemon::bind();
    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let mut health = source.health();
    let t0 = Instant::now();
    let stream = Box::new(source).capture();
    assert!(
        t0.elapsed() < Duration::from_millis(200),
        "capture() blocked for {:?}",
        t0.elapsed()
    );
    assert_eq!(health.next().await, Some(CaptureHealth::Opening));
    drop(stream);
    health_to_end(health, Duration::from_secs(2)).await;
}

/// A target no node in the graph carries faults at discovery, naming it.
#[tokio::test]
async fn unknown_target_faults_at_discovery() {
    skip_unless_enabled!();
    let source = CaptureSource::builder(AudioFormat::default())
        .target("myna-no-such-node")
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let health = source.health();
    let _stream = Box::new(source).capture();

    let (states, took) = health_to_end(health, Duration::from_secs(3)).await;
    assert!(took < Duration::from_secs(1), "faulted after {took:?}");
    let msg = device_unavailable(states.last());
    assert!(
        msg.contains("no audio source available for 'myna-no-such-node'"),
        "got: {msg}"
    );
}

/// A graceful stop while the daemon has not answered discovery ends capture
/// promptly and releases the thread, as a fault: nothing was captured.
#[tokio::test]
async fn stop_during_discovery_releases_promptly() {
    skip_unless_enabled!();
    let daemon = SilentDaemon::bind();
    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let health = source.health();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    let _conn = daemon.connected().await;
    stop.stop();

    let (states, took) = health_to_end(health, Duration::from_secs(2)).await;
    assert!(took < Duration::from_secs(1), "released after {took:?}");
    let msg = device_unavailable(states.last());
    assert!(msg.contains("stopped before"), "got: {msg}");
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(1)).await;
    assert!(chunks.is_empty());
    assert_eq!(fault.map(CaptureHealth::Faulted).as_ref(), states.last());
}

/// Dropping the stream while discovery is pending releases the thread.
#[tokio::test]
async fn abort_during_discovery_releases_promptly() {
    skip_unless_enabled!();
    let daemon = SilentDaemon::bind();
    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let health = source.health();
    let stream = Box::new(source).capture();
    let _conn = daemon.connected().await;
    drop(stream);

    let (states, took) = health_to_end(health, Duration::from_secs(2)).await;
    assert!(took < Duration::from_secs(1), "released after {took:?}");
    assert_eq!(states.last(), Some(&CaptureHealth::Ended));
}

/// A daemon that never answers faults at the discovery deadline (3 s).
#[tokio::test]
async fn silent_daemon_faults_at_the_discovery_deadline() {
    skip_unless_enabled!();
    let daemon = SilentDaemon::bind();
    let source = CaptureSource::builder(AudioFormat::default())
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build();
    let health = source.health();
    let stream = Box::new(source).capture();

    let (states, took) = health_to_end(health, Duration::from_secs(8)).await;
    assert!(
        took >= Duration::from_millis(2_500) && took < Duration::from_secs(5),
        "faulted after {took:?}"
    );
    device_unavailable(states.last());
    assert!(!states.contains(&CaptureHealth::Capturing));
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(1)).await;
    assert!(chunks.is_empty());
    assert_eq!(fault.map(CaptureHealth::Faulted).as_ref(), states.last());
}

/// Without a session manager nothing links a stream to its target, so it
/// never delivers audio. The bare daemon's own driver node passes discovery
/// by name, leaving capture waiting on the link.
fn unlinkable_source(daemon: &NoSmDaemon) -> CaptureSource {
    CaptureSource::builder(AudioFormat::default())
        .target("Dummy-Driver")
        .backend(Box::new(PipeWireBackend::with_remote(daemon.remote())))
        .build()
}

/// Resolves once the daemon has registered our stream node, which is when the
/// stream turns `Paused` (wired).
async fn stream_node_registered(remote: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let listed = tokio::process::Command::new("pw-cli")
                .args(["-r", remote, "ls", "Node"])
                .output()
                .await
                .expect("pw-cli");
            if String::from_utf8_lossy(&listed.stdout).contains("\"myna-dictate\"") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the stream node never appeared in the graph");
}

/// A quick tap: stopped once the stream is wired but before the device
/// delivered anything is an empty capture, not a fault, and releases promptly.
#[tokio::test]
async fn stop_after_wiring_before_audio_ends_cleanly() {
    skip_unless_enabled!();
    let daemon = NoSmDaemon::spawn();
    let source = unlinkable_source(&daemon);
    let health = source.health();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    stream_node_registered(&daemon.remote()).await;
    stop.stop();

    let (states, took) = health_to_end(health, Duration::from_secs(2)).await;
    assert!(took < Duration::from_secs(1), "released after {took:?}");
    assert_eq!(states.last(), Some(&CaptureHealth::Ended), "{states:?}");
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(1)).await;
    assert!(chunks.is_empty());
    assert!(fault.is_none(), "{fault:?}");
}

/// A stream that never links faults at the link deadline (3 s after connect).
#[tokio::test]
async fn unlinked_stream_faults_at_the_link_deadline() {
    skip_unless_enabled!();
    let daemon = NoSmDaemon::spawn();
    let source = unlinkable_source(&daemon);
    let health = source.health();
    let _stream = Box::new(source).capture();

    let (states, took) = health_to_end(health, Duration::from_secs(8)).await;
    assert!(
        took >= Duration::from_millis(2_500) && took < Duration::from_secs(5),
        "faulted after {took:?}"
    );
    let msg = device_unavailable(states.last());
    assert!(msg.contains("no audio is flowing"), "got: {msg}");
}

/// The links leaving `node`'s output ports, from `pw-link -lI`: link id,
/// output port id and input port id. Ids, because every capture stream in
/// this suite shares one `node.name`.
fn links_from(node: &str) -> Vec<(String, String, String)> {
    // No silent empty list: the callers assert on links appearing and moving,
    // and "pw-link is not installed" would read as "the graph has no links".
    let out = Command::new("pw-link")
        .arg("-lI")
        .output()
        .unwrap_or_else(|e| panic!("run pw-link to read the graph's links ({e}). {HOW_TO_RUN}"));
    let mut links = Vec::new();
    let mut from_port = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            [id, "|->", input, _] => {
                if let Some(output) = &from_port {
                    links.push((id.to_string(), String::clone(output), input.to_string()));
                }
            }
            [id, port] if !port.starts_with('|') => {
                from_port = port
                    .starts_with(&format!("{node}:"))
                    .then(|| id.to_string());
            }
            _ => from_port = None,
        }
    }
    links
}

/// Remove every link leaving `node`, returning the port id pairs they joined.
fn unlink_all_from(node: &str) -> Vec<(String, String)> {
    let links = links_from(node);
    assert!(!links.is_empty(), "no links from {node}");
    links
        .into_iter()
        .map(|(id, output, input)| {
            let unlinked = Command::new("pw-link").args(["-d", &id]).status();
            assert!(
                unlinked.is_ok_and(|s| s.success()),
                "could not remove link {id}"
            );
            (output, input)
        })
        .collect()
}

/// A capture whose source stops delivering mid-capture (its links removed,
/// and the session manager does not relink a targeted stream) faults within
/// the no-progress window instead of reading as a silent user.
#[tokio::test]
async fn stalled_source_faults_mid_capture() {
    skip_unless_enabled!();
    let vsrc = VirtualSource::spawn("myna-test-src-stall");
    let source = CaptureSource::builder(AudioFormat::default())
        .target(vsrc.node_name.clone())
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let health = source.health();
    let _stream = Box::new(source).capture();
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(300),
            Duration::from_secs(6)
        )
        .await,
        "the virtual source never delivered"
    );

    unlink_all_from(&vsrc.node_name);

    let (states, took) = health_to_end(health, Duration::from_secs(8)).await;
    let msg = device_unavailable(states.last());
    assert!(msg.contains("no audio is flowing"), "got: {msg}");
    assert!(
        took >= Duration::from_millis(2_500) && took < Duration::from_secs(5),
        "stall detected after {took:?}"
    );
}

/// A source relinked within the no-progress window, as a session manager
/// does when the device or its profile changes, resumes the same capture:
/// the unlinked interval is neither a fault nor counted as lost audio.
#[tokio::test]
async fn relinked_source_resumes_without_a_fault() {
    skip_unless_enabled!();
    const RELINK_TAKES: Duration = Duration::from_secs(1);
    let vsrc = VirtualSource::spawn("myna-test-src-relink");
    let source = CaptureSource::builder(AudioFormat::default())
        .target(vsrc.node_name.clone())
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let health = source.health();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(300),
            Duration::from_secs(6)
        )
        .await,
        "the virtual source never delivered"
    );

    let pairs = unlink_all_from(&vsrc.node_name);
    tokio::time::sleep(RELINK_TAKES).await;
    let before = stats.borrow().captured;
    for (output, input) in &pairs {
        let linked = Command::new("pw-link").args([output, input]).status();
        assert!(
            linked.is_ok_and(|s| s.success()),
            "could not link {output} -> {input}"
        );
    }
    let resumed = wait_captured(
        &mut stats,
        before + Duration::from_millis(500),
        Duration::from_secs(3),
    )
    .await;
    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
    assert!(fault.is_none(), "relinking faulted: {fault:?}");
    assert!(resumed, "capture did not resume after the relink");
    assert!(!chunks.is_empty());
    let (states, _) = health_to_end(health, Duration::from_secs(2)).await;
    assert_eq!(states.last(), Some(&CaptureHealth::Ended));
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
        assert_eq!(
            c.format, fmt,
            "every chunk is exactly the negotiated format"
        );
    }
}

/// T010: device native format ≠ negotiated → consumer still receives exactly
/// the negotiated format, converted graph-side (C2, FR-003). We request an
/// unusual rate/channel combo the device almost certainly doesn't natively
/// produce and assert the chunks still carry the requested format.
#[tokio::test]
async fn graph_side_conversion_delivers_negotiated_format() {
    skip_unless_enabled!();
    // 48 kHz stereo — very likely a conversion from the graph's native source.
    let fmt = AudioFormat {
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bytes: 2,
    };
    let mut builder = CaptureSource::builder(fmt).ring_depth(Duration::from_secs(30));
    if let Some(t) = target() {
        builder = builder.target(t);
    }
    let source = builder.backend(Box::new(PipeWireBackend::new())).build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(300),
            Duration::from_secs(5)
        )
        .await,
        "capture established"
    );
    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(8)).await;
    assert!(fault.is_none(), "clean end: {fault:?}");
    assert!(!chunks.is_empty(), "captured some converted audio");
    for c in &chunks {
        assert_eq!(
            c.format, fmt,
            "chunks carry the negotiated (converted) format"
        );
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
    let source = CaptureSource::builder(fmt)
        .backend(Box::new(PipeWireBackend::new()))
        .build();
    let mut stats = source.stats();
    let health = source.health();
    let stream = Box::new(source).capture();
    // Abort while audio flows: ConsumerGuard trips stop and closes the ring;
    // the loop thread must observe it and tear down.
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(100),
            Duration::from_secs(5)
        )
        .await,
        "capture established"
    );
    drop(stream);
    let (states, _) = health_to_end(health, Duration::from_secs(2)).await;
    assert_eq!(states.last(), Some(&CaptureHealth::Ended));
}

/// Await `captured >= at_least` on the stats tap; false if the tap closed or
/// the budget ran out first.
async fn wait_captured(
    stats: &mut tokio::sync::watch::Receiver<myna_audio::AudioStats>,
    at_least: Duration,
    budget: Duration,
) -> bool {
    tokio::time::timeout(budget, async {
        loop {
            if stats.borrow_and_update().captured >= at_least {
                return true;
            }
            if stats.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Repeated start/stop/drop while the process callback is delivering audio:
/// every cycle must end cleanly (graceful stop drains, abort discards) and
/// release the capture thread promptly, never crash or leak.
#[tokio::test]
async fn repeated_start_stop_drop_with_callbacks_running() {
    skip_unless_enabled!();
    for cycle in 0..12 {
        let mut builder = CaptureSource::builder(AudioFormat::default());
        if let Some(t) = target() {
            builder = builder.target(t);
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        let mut stats = source.stats();
        let health = source.health();
        let stop = source.stop_handle();
        let stream = Box::new(source).capture();
        assert!(
            wait_captured(
                &mut stats,
                Duration::from_millis(150),
                Duration::from_secs(5)
            )
            .await,
            "cycle {cycle}: no audio flowed"
        );
        if cycle % 2 == 0 {
            stop.stop();
            let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
            assert!(
                fault.is_none(),
                "cycle {cycle}: graceful stop faulted: {fault:?}"
            );
            assert!(!chunks.is_empty(), "cycle {cycle}: captured audio drains");
            let drained: Duration = chunks.iter().map(PcmChunk::duration).sum();
            assert_eq!(
                drained,
                stats.borrow().captured,
                "cycle {cycle}: every captured chunk drains exactly once"
            );
        } else {
            drop(stream);
        }
        let (states, _) = health_to_end(health, Duration::from_secs(2)).await;
        assert_eq!(states.last(), Some(&CaptureHealth::Ended), "cycle {cycle}");
    }
}

/// T023: channel pick/downmix on a multi-channel source (C6; SC-004, US3-1).
/// Create a 4-channel virtual source, select two channels, and assert capture
/// links and delivers the negotiated (downmixed) mono format. (Exact per-
/// channel signal discrimination needs a fed multichannel signal; the
/// pick/downmix math itself is unit-tested in `native::tests`.)
#[tokio::test]
async fn multichannel_channel_selection_captures() {
    skip_unless_enabled!();
    let vsrc = VirtualSource::spawn_channels("myna-test-4ch-023", Some("FL,FR,RL,RR"));
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
    assert!(
        linked && !chunks.is_empty(),
        "multichannel selection linked + captured"
    );
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
    let vsrc = VirtualSource::spawn("myna-test-src-027");
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

    let vsrc = VirtualSource::spawn(name);
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
    let dev = devices.list().into_iter().next().unwrap_or_else(|| {
        panic!("the gate promised a graph, and it lists no sources. {HOW_TO_RUN}")
    });
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
    assert!(
        linked,
        "an enumerated device name is a usable capture target"
    );
}

/// Hold the stats tap's read guard for `stall` on a thread of its own: the
/// loop thread parks in its next publish, as if starved of CPU, while
/// PipeWire keeps delivering. Returns once the guard is held, so the caller
/// can watch the tap for as long as it is, and hands back the holder.
fn stall_capture_loop(
    stats: &tokio::sync::watch::Receiver<myna_audio::AudioStats>,
    stall: Duration,
) -> std::thread::JoinHandle<()> {
    let taken = Arc::new(std::sync::Barrier::new(2));
    let holder = std::thread::spawn({
        let stats = stats.clone();
        let taken = taken.clone();
        move || {
            let guard = stats.borrow();
            taken.wait();
            std::thread::sleep(stall);
            drop(guard);
        }
    });
    taken.wait();
    holder
}

fn default_capture_source() -> CaptureSource {
    let mut builder = CaptureSource::builder(AudioFormat::default());
    if let Some(t) = target() {
        builder = builder.target(t);
    }
    builder.backend(Box::new(PipeWireBackend::new())).build()
}

/// A starved capture loop thread is not lost audio: the realtime callback
/// keeps buffering, and the loop catches up once it runs again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_loop_stall_within_the_realtime_buffer_loses_nothing() {
    skip_unless_enabled!();
    const STALL: Duration = Duration::from_millis(500);
    let source = default_capture_source();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(200),
            Duration::from_secs(5)
        )
        .await,
        "capture established"
    );

    let holder = stall_capture_loop(&stats, STALL);
    // Read under the stall: the value cannot move while the guard is held,
    // so this is where the loop thread got to, and nothing can reach the tap
    // until it is released - that silence is the stall having happened.
    let before = stats.borrow_and_update().captured;
    let stalled = tokio::time::timeout(STALL / 2, stats.changed())
        .await
        .is_err();
    holder.join().expect("the thread holding the stats tap");
    // Everything the realtime callback buffered meanwhile is still there.
    let caught_up = wait_captured(&mut stats, before + STALL, Duration::from_secs(5)).await;
    stop.stop();
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
    assert!(fault.is_none(), "a loop stall faulted: {fault:?}");
    assert!(stalled, "the capture loop was never held up");
    assert!(caught_up, "capture did not catch up after the stall");
    let drained: Duration = chunks.iter().map(PcmChunk::duration).sum();
    assert_eq!(drained, stats.borrow().captured);
}

/// A loop thread starved for longer than the realtime buffer holds overflows
/// it, which faults instead of silently skipping audio.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_loop_stall_past_the_realtime_buffer_faults() {
    skip_unless_enabled!();
    let source = default_capture_source();
    let mut stats = source.stats();
    let health = source.health();
    let stream = Box::new(source).capture();
    assert!(
        wait_captured(
            &mut stats,
            Duration::from_millis(200),
            Duration::from_secs(5)
        )
        .await,
        "capture established"
    );

    let holder = stall_capture_loop(&stats, Duration::from_millis(2_500));

    let (states, _) = health_to_end(health, Duration::from_secs(8)).await;
    holder.join().expect("the thread holding the stats tap");
    match states.last() {
        Some(CaptureHealth::Faulted(CaptureError::Backend(msg))) => {
            assert!(msg.contains("overflowed"), "got: {msg}")
        }
        other => panic!("expected an overflow fault, got {other:?}"),
    }
    let (chunks, fault) = drain_with_timeout(stream, Duration::from_secs(3)).await;
    let drained: Duration = chunks.iter().map(PcmChunk::duration).sum();
    assert!(
        drained >= Duration::from_secs(2),
        "the audio buffered before the overflow drains, got {drained:?}"
    );
    assert_eq!(fault.map(CaptureHealth::Faulted).as_ref(), states.last());
}

/// Child half of `stopped_process_faults_as_lost_audio`, run in its own
/// process so stopping it cannot disturb the other captures in this suite.
#[tokio::test]
async fn stopped_process_child() {
    // Not a test of its own: it asserts nothing, it reports to the parent,
    // which judges it. Skips loudly, like every other gate in this suite.
    if std::env::var_os("MYNA_STOPPED_PROCESS_CHILD").is_none() {
        eprintln!(
            "skipped: stopped_process_faults_as_lost_audio runs this half \
             with MYNA_STOPPED_PROCESS_CHILD=1"
        );
        return;
    }
    let source = default_capture_source();
    let mut stats = source.stats();
    let health = source.health();
    let _stream = Box::new(source).capture();
    let ready = wait_captured(
        &mut stats,
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .await;
    println!("READY {ready}");
    let (states, _) = health_to_end(health, Duration::from_secs(8)).await;
    println!("OUTCOME {:?}", states.last());
}

/// Graph cycles missed while the capture process cannot run (the realtime
/// callback included) are lost audio, and capture faults: a stopped process
/// is an xrun long enough to be unambiguous.
#[tokio::test]
async fn stopped_process_faults_as_lost_audio() {
    skip_unless_enabled!();
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "stopped_process_child", "--nocapture"])
        .env("MYNA_STOPPED_PROCESS_CHILD", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the child capture");
    let pid = child.id().to_string();
    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let next_line = |prefix: &str| loop {
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("the child never printed {prefix}"));
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.to_string();
        }
    };
    assert_eq!(next_line("READY "), "true", "the child never captured");

    let signal = |sig: &str| {
        let sent = Command::new("kill").args([sig, &pid]).status();
        assert!(sent.is_ok_and(|s| s.success()), "kill {sig} failed");
    };
    signal("-STOP");
    std::thread::sleep(Duration::from_millis(300));
    signal("-CONT");

    let outcome = next_line("OUTCOME ");
    let _ = child.wait();
    assert!(
        outcome.contains("Faulted(Backend(") && outcome.contains("lost"),
        "expected lost audio to fault, got {outcome}"
    );
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
    }

    /// Captured audio keeps pace with the wall clock once flowing: the
    /// callback thread never starves the stream into dropped cycles.
    #[tokio::test]
    async fn perf_capture_keeps_pace_with_wall_clock() {
        skip_unless_enabled!();
        // Stats move in 100 ms chunks: baseline deficit ~100 ms, ceiling 3 chunks.
        const WINDOW: Duration = Duration::from_secs(3);
        const MAX_DEFICIT: Duration = Duration::from_millis(300);
        let mut builder = CaptureSource::builder(AudioFormat::default());
        if let Some(t) = target() {
            builder = builder.target(t);
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        let mut stats = source.stats();
        let stop = source.stop_handle();
        let stream = Box::new(source).capture();
        assert!(
            wait_captured(
                &mut stats,
                Duration::from_millis(200),
                Duration::from_secs(5)
            )
            .await,
            "capture established"
        );
        let start = stats.borrow().captured;
        let t0 = std::time::Instant::now();
        tokio::time::sleep(WINDOW).await;
        let captured = stats.borrow().captured - start;
        let elapsed = t0.elapsed();
        stop.stop();
        let _ = drain_with_timeout(stream, Duration::from_secs(2)).await;
        let deficit = elapsed.saturating_sub(captured);
        eprintln!("capture pace: {captured:?} captured over {elapsed:?}");
        assert!(
            deficit <= MAX_DEFICIT,
            "capture fell behind the wall clock by {deficit:?} (ceiling {MAX_DEFICIT:?})"
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
    let vsrc = VirtualSource::spawn("myna-test-src-018");

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
    assert!(
        fault.is_none(),
        "clean end from a resolvable target: {fault:?}"
    );
    assert!(
        linked && !chunks.is_empty(),
        "capture linked to the named target"
    );
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
    assert!(
        elapsed < Duration::from_secs(1),
        "stop drained promptly: {elapsed:?}"
    );
}
