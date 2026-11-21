use std::future::{poll_fn, Future};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::task::{Poll, Waker};

use async_trait::async_trait;
use myna_config::adapters::snap_backend::SnapBackendRepository;
use myna_config::adapters::snapd_client::{SnapdError, SnapdReads};
use myna_config::command::{
    CancellationToken, CommandError, CommandOutput, CommandRequest, CommandRunner,
    FakeCommandRunner,
};
use myna_config::diagnostics::{InstalledSnap, BACKEND_REFRESH_PROCESS_BUDGET};
use myna_config::domain::{
    ActiveBackendState, BackendIdentity, BackendSurface, ConfigValue, ContentConnections,
};
use myna_config::ports::BackendRepository;

/// `GET /v2/snaps` and `GET /v2/connections?select=all&interface=content`
/// results from a VM with Parakeet connected and Whisper installed, trimmed.
const SNAPS: &str = include_str!("fixtures/snapd-snaps.json");
const CONNECTIONS: &str = include_str!("fixtures/snapd-connections.json");
const GET: &str = include_str!("fixtures/modelctl-get.txt");
const VERSION: &str = include_str!("fixtures/modelctl-version.json");
const STATUS: &str = include_str!("fixtures/modelctl-status.json");
const MODELS: &str = include_str!("fixtures/modelctl-list-models.json");
const ENGINES: &str = include_str!("fixtures/modelctl-list-engines.json");
const PARAKEET_INFO: &str = include_str!("fixtures/snap-info-parakeet.txt");
const UNUSUAL_INFO: &str = include_str!("fixtures/snap-info-unusual-app.txt");
const MULTI_MODELS: &str = include_str!("fixtures/modelctl-list-models-multi.json");
const MULTI_ENGINES: &str = include_str!("fixtures/modelctl-list-engines-multi.json");

fn ok(stdout: &str) -> Result<CommandOutput, CommandError> {
    Ok(CommandOutput::new(Some(0), stdout, ""))
}

fn failed(command: &str) -> Result<CommandOutput, CommandError> {
    Err(CommandError::NonZero {
        exit_status: Some(1),
        stdout: String::new(),
        stderr: format!("{command} failed"),
    })
}

/// snapd answering every read with these results, counting the reads.
struct FakeSnapd {
    snaps: Result<Vec<InstalledSnap>, SnapdError>,
    connections: Result<ContentConnections, SnapdError>,
    reads: Mutex<usize>,
}

impl FakeSnapd {
    fn answering(
        snaps: Result<Vec<InstalledSnap>, SnapdError>,
        connections: Result<ContentConnections, SnapdError>,
    ) -> Self {
        Self {
            snaps,
            connections,
            reads: Mutex::new(0),
        }
    }

    fn reads(&self) -> usize {
        *self.reads.lock().unwrap()
    }
}

impl Default for FakeSnapd {
    fn default() -> Self {
        Self::answering(
            Ok(serde_json::from_str(SNAPS).unwrap()),
            Ok(serde_json::from_str(CONNECTIONS).unwrap()),
        )
    }
}

#[async_trait(?Send)]
impl SnapdReads for FakeSnapd {
    async fn installed_snaps(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<InstalledSnap>, SnapdError> {
        *self.reads.lock().unwrap() += 1;
        self.snaps.clone()
    }

    async fn content_connections(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<ContentConnections, SnapdError> {
        *self.reads.lock().unwrap() += 1;
        self.connections.clone()
    }
}

fn repository(
    outcomes: impl IntoIterator<Item = Result<CommandOutput, CommandError>>,
) -> (SnapBackendRepository, FakeCommandRunner) {
    let runner = FakeCommandRunner::scripted(outcomes);
    (
        SnapBackendRepository::new(Arc::new(runner.clone()), Arc::new(FakeSnapd::default())),
        runner,
    )
}

fn snapd_repository(
    snapd: FakeSnapd,
) -> (SnapBackendRepository, Arc<FakeSnapd>, FakeCommandRunner) {
    let runner = FakeCommandRunner::default();
    let snapd = Arc::new(snapd);
    (
        SnapBackendRepository::new(Arc::new(runner.clone()), snapd.clone()),
        snapd,
        runner,
    )
}

fn snapd_refused() -> SnapdError {
    SnapdError::Transport {
        message: "connection refused".to_owned(),
    }
}

/// snap translates its tables' headers, so the inventory and discovery read
/// snapd's JSON and never parse the `snap` CLI.
#[test]
fn installed_snap_inventory_reads_snapd_once_and_preserves_versions() {
    let (repository, snapd, runner) = snapd_repository(FakeSnapd::default());

    let snaps =
        block_on(repository.installed_snaps(CancellationToken::new())).expect("snap inventory");

    let names: Vec<_> = snaps
        .iter()
        .map(|snap| (snap.name.as_str(), snap.version.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("myna-parakeet", "0.1.0+git183.6fd73f42"),
            ("myna-whisper", "0.1.0+git183.6fd73f42"),
            ("myna", "0.2.1"),
            ("snapd", "2.77.1"),
        ]
    );
    assert_eq!(snapd.reads(), 1);
    assert!(runner.calls().is_empty(), "ran {:?}", runner.calls());
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    gio::glib::MainContext::new().block_on(future)
}

fn argv(call: &CommandRequest) -> (&str, Vec<&str>) {
    (
        call.executable(),
        call.arguments().iter().map(String::as_str).collect(),
    )
}

#[test]
fn discovers_installed_connected_and_unconnected_backends_once() {
    let (repository, snapd, runner) = snapd_repository(FakeSnapd::default());

    let discovered =
        block_on(repository.discover(CancellationToken::new())).expect("discovery succeeds");

    assert_eq!(
        discovered.backends(),
        &[
            BackendIdentity::new("myna-parakeet", "provider"),
            BackendIdentity::new("myna-whisper", "provider")
        ]
    );
    assert_eq!(
        discovered.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider"))
    );
    assert_eq!(snapd.reads(), 1);
    assert!(runner.calls().is_empty(), "ran {:?}", runner.calls());
}

#[test]
fn discovery_reads_myna_role_from_provider_env_to_partition_speech_and_text_snaps() {
    let dir =
        std::env::temp_dir().join(format!("myna-config-role-discovery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let write_env = |snap: &str, sub: &str, body: &str| {
        let target = dir.join(snap).join(sub);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("provider.env"), body).unwrap();
    };
    write_env(
        "myna-nemotron",
        "common/share/provider",
        "SNAP_NAME=myna-nemotron\nSNAP_INSTANCE_NAME=myna-nemotron\nUNIX_SOCKET=myna.sock\nMYNA_ROLE=speech\n",
    );
    write_env(
        "myna-text",
        "common/share/provider",
        "SNAP_NAME=myna-text\nSNAP_INSTANCE_NAME=myna-text\nUNIX_SOCKET=text.sock\nMYNA_ROLE=text\n",
    );

    let connections: ContentConnections = serde_json::from_value(serde_json::json!({
        "slots": [
            {
                "snap": "myna-nemotron",
                "slot": "provider",
                "interface": "content",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "write": ["$SNAP_COMMON/share/provider"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            },
            {
                "snap": "myna-text",
                "slot": "provider",
                "interface": "content",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "write": ["$SNAP_COMMON/share/provider"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            },
            {
                "snap": "gemma4",
                "slot": "provider",
                "interface": "content",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "read": ["$SNAP_COMMON/share/provider"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            }
        ]
    }))
    .unwrap();

    let runner = FakeCommandRunner::default();
    let snapd = Arc::new(FakeSnapd::answering(Ok(Vec::new()), Ok(connections)));
    let repository = SnapBackendRepository::with_snap_root(Arc::new(runner.clone()), snapd, &dir);

    let discovered =
        block_on(repository.discover(CancellationToken::new())).expect("discovery succeeds");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        discovered.backends(),
        &[BackendIdentity::new("myna-nemotron", "provider")]
    );
    assert_eq!(
        discovered.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-nemotron", "provider"))
    );
    assert_eq!(
        discovered.text_providers(),
        &[
            BackendIdentity::new("gemma4", "provider"),
            BackendIdentity::new("myna-text", "provider"),
        ]
    );
    assert!(discovered.strays().is_empty());
    assert!(runner.calls().is_empty());
}

#[test]
fn no_installed_backends_is_a_successful_empty_discovery() {
    let (repository, _, _) = snapd_repository(FakeSnapd::answering(
        Ok(Vec::new()),
        Ok(ContentConnections::default()),
    ));

    let discovered = block_on(repository.discover(CancellationToken::new())).unwrap();

    assert!(discovered.backends().is_empty());
    assert_eq!(discovered.active_state(), ActiveBackendState::Disconnected);
}

#[test]
fn discovery_inventory_and_installed_app_listing_fail_explicitly() {
    let (refused, _, _) = snapd_repository(FakeSnapd::answering(
        Err(snapd_refused()),
        Err(snapd_refused()),
    ));
    let discovery = block_on(refused.discover(CancellationToken::new())).unwrap_err();
    assert_eq!(discovery.surface(), BackendSurface::Connections);
    assert_eq!(
        discovery.message(),
        "snapd transport error: connection refused"
    );
    let inventory = block_on(refused.installed_snaps(CancellationToken::new())).unwrap_err();
    assert_eq!(inventory.surface(), BackendSurface::SnapInventory);
    assert_eq!(
        inventory.message(),
        "snapd transport error: connection refused"
    );

    let (repository, runner) = repository([failed("snap info")]);
    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("myna-parakeet", "provider"),
        CancellationToken::new(),
    ));
    assert_eq!(snapshot.errors().len(), 1);
    assert!(snapshot.error(BackendSurface::ModelctlApp).is_some());
    assert_eq!(runner.calls().len(), 1);
}

#[test]
fn reads_the_installed_parakeet_shape_without_per_setting_commands() {
    let (repository, runner) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("myna-parakeet", "provider"),
        CancellationToken::new(),
    ));

    assert_eq!(
        snapshot.identity().modelctl_app(),
        Some("myna-parakeet.parakeet")
    );
    assert!(snapshot.errors().is_empty());
    assert_eq!(
        snapshot.models().unwrap().active(),
        Some("parakeet-tdt-0.6b-v3")
    );
    assert_eq!(snapshot.engines().unwrap().active(), Some("cpu"));
    assert_eq!(
        snapshot.configuration().get("ws.unix-socket"),
        Some(&ConfigValue::Text(
            "/var/snap/myna-parakeet/common/share/provider/myna.sock".into()
        ))
    );
    assert_eq!(runner.calls().len(), 6);
    assert_eq!(
        runner.calls().iter().map(argv).collect::<Vec<_>>(),
        [
            ("snap", vec!["info", "myna-parakeet"]),
            (
                "snap",
                vec!["run", "myna-parakeet.parakeet", "version", "--format=json"]
            ),
            (
                "snap",
                vec!["run", "myna-parakeet.parakeet", "status", "--format=json"]
            ),
            ("snap", vec!["run", "myna-parakeet.parakeet", "get"]),
            (
                "snap",
                vec![
                    "run",
                    "myna-parakeet.parakeet",
                    "list-models",
                    "--format=json"
                ]
            ),
            (
                "snap",
                vec![
                    "run",
                    "myna-parakeet.parakeet",
                    "list-engines",
                    "--format=json"
                ]
            ),
        ]
    );
}

#[test]
fn reads_multi_engine_model_shape() {
    let (repository, _) = repository([
        ok(UNUSUAL_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MULTI_MODELS),
        ok(MULTI_ENGINES),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("community-asr", "provider"),
        CancellationToken::new(),
    ));

    assert_eq!(
        snapshot.identity().modelctl_app(),
        Some("community-asr.control-centre")
    );
    assert_eq!(snapshot.models().unwrap().options().len(), 3);
    assert_eq!(snapshot.engines().unwrap().options().len(), 2);
    assert_eq!(
        snapshot.configuration().get("shared"),
        Some(&ConfigValue::Text("user".into()))
    );
}

#[test]
fn modelctl_values_are_the_configuration() {
    let modelctl = "\
stream-arm-seconds: 21
streaming: false
verbose: false
ws.unix-socket: /var/snap/myna-parakeet/common/share/provider/custom.sock
";
    let (repository, _) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(modelctl),
        ok(MODELS),
        ok(ENGINES),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("myna-parakeet", "provider"),
        CancellationToken::new(),
    ));
    let configuration = snapshot.configuration();

    assert_eq!(
        configuration.get("streaming"),
        Some(&ConfigValue::Boolean(false))
    );
    assert_eq!(
        configuration.get("stream-arm-seconds"),
        Some(&ConfigValue::Integer(21))
    );
    assert_eq!(
        configuration.get("ws.unix-socket"),
        Some(&ConfigValue::Text(
            "/var/snap/myna-parakeet/common/share/provider/custom.sock".into()
        ))
    );
}

#[test]
fn cached_apps_are_verified_and_invalidated_on_failure_and_refresh() {
    let (repository, runner) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
        failed("cached version"),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
    ]);
    let backend = BackendIdentity::new("myna-parakeet", "provider");

    let first = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(first.errors().is_empty());
    let failed_cached = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(failed_cached.error(BackendSurface::ModelctlApp).is_some());
    assert!(failed_cached
        .error(BackendSurface::ModelctlConfig)
        .is_none());
    assert!(failed_cached.models().is_some());
    assert!(failed_cached.engines().is_some());
    let recovered = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(recovered.errors().is_empty());

    block_on(repository.refresh(CancellationToken::new())).unwrap();
    let after_refresh = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(after_refresh.errors().is_empty());

    let info_calls = runner
        .calls()
        .iter()
        .filter(|call| call.arguments().first().map(String::as_str) == Some("info"))
        .count();
    assert_eq!(info_calls, 3);
}

fn refused(stderr: &str) -> Result<CommandOutput, CommandError> {
    Err(CommandError::NonZero {
        exit_status: Some(1),
        stdout: String::new(),
        stderr: stderr.into(),
    })
}

#[test]
fn a_backend_with_no_engine_selected_is_a_state_not_a_failure() {
    let (repository, _) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        refused("Error: getting json status: getting status: no active engine\n"),
        ok(GET),
        refused("Error: loading engine manifest: engine manifest not found\n"),
        ok(ENGINES),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("myna-parakeet", "provider"),
        CancellationToken::new(),
    ));

    assert!(snapshot.errors().is_empty());
    assert!(snapshot.status().is_none());
    assert!(snapshot.models().is_none());
    assert!(snapshot.engines().is_some());
}

/// A sideloaded backend without hardware-observe: Diagnostics showed
/// "command exited unsuccessfully with status Some(1)" and nothing else.
#[test]
fn a_failed_surface_carries_what_modelctl_said() {
    let (repository, _) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        refused("Error: listing engines: open /sys/bus/usb/devices: permission denied\n"),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("myna-parakeet", "provider"),
        CancellationToken::new(),
    ));

    let message = snapshot
        .error(BackendSurface::Engines)
        .expect("list-engines failed")
        .message();
    assert_eq!(
        message,
        "command exited with status 1: Error: listing engines: \
         open /sys/bus/usb/devices: permission denied"
    );
}

#[test]
fn likely_modelctl_command_after_irrelevant_apps_is_probed_within_budget() {
    let info = "\
name:      community-asr
commands:
  - community-asr.docs
  - community-asr.export
  - community-asr.import
  - community-asr.monitor
  - community-asr.logs
  - community-asr.modelctl
installed: 2.0
";
    let (repository, runner) = repository([
        ok(info),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("community-asr", "provider"),
        CancellationToken::new(),
    ));

    assert!(snapshot.errors().is_empty());
    assert_eq!(
        snapshot.identity().modelctl_app(),
        Some("community-asr.modelctl")
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 6);
    assert_eq!(
        argv(&calls[1]),
        (
            "snap",
            vec!["run", "community-asr.modelctl", "version", "--format=json"]
        )
    );
}

#[test]
fn modelctl_discovery_never_probes_more_than_four_advertised_commands() {
    let info = "\
name:      community-asr
commands:
  - community-asr.one
  - community-asr.two
  - community-asr.three
  - community-asr.four
  - community-asr.five
installed: 2.0
";
    let (repository, runner) = repository([
        ok(info),
        failed("one"),
        failed("two"),
        failed("three"),
        failed("four"),
    ]);

    let snapshot = block_on(repository.read_snapshot(
        &BackendIdentity::new("community-asr", "provider"),
        CancellationToken::new(),
    ));

    assert!(snapshot.error(BackendSurface::ModelctlApp).is_some());
    let calls = runner.calls();
    assert_eq!(calls.len(), 5);
    assert!(calls.len() <= BACKEND_REFRESH_PROCESS_BUDGET);
    assert!(!calls.iter().any(|call| {
        call.arguments()
            .get(1)
            .is_some_and(|argument| argument == "community-asr.five")
    }));
}

#[test]
fn any_modelctl_surface_failure_invalidates_the_verified_app() {
    let (repository, runner) = repository([
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        failed("get"),
        ok(MODELS),
        ok(ENGINES),
        ok(PARAKEET_INFO),
        ok(VERSION),
        ok(STATUS),
        ok(GET),
        ok(MODELS),
        ok(ENGINES),
    ]);
    let backend = BackendIdentity::new("myna-parakeet", "provider");

    let partial = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(partial.error(BackendSurface::ModelctlConfig).is_some());
    let recovered = block_on(repository.read_snapshot(&backend, CancellationToken::new()));
    assert!(recovered.errors().is_empty());

    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| call.arguments().first().map(String::as_str) == Some("info"))
            .count(),
        2
    );
}

#[derive(Clone)]
struct RefreshRaceRunner {
    state: Arc<RefreshRaceState>,
}

#[derive(Default)]
struct RefreshRaceState {
    calls: Mutex<Vec<CommandRequest>>,
    status_started: AtomicBool,
    release_status: AtomicBool,
    status_waker: Mutex<Option<Waker>>,
}

impl RefreshRaceRunner {
    fn new() -> Self {
        Self {
            state: Arc::new(RefreshRaceState::default()),
        }
    }

    fn calls(&self) -> Vec<CommandRequest> {
        self.state.calls.lock().unwrap().clone()
    }

    fn release_status(&self) {
        self.state.release_status.store(true, Ordering::SeqCst);
        if let Some(waker) = self.state.status_waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

#[async_trait(?Send)]
impl CommandRunner for RefreshRaceRunner {
    async fn run(
        &self,
        request: CommandRequest,
        _cancellation: CancellationToken,
    ) -> Result<CommandOutput, CommandError> {
        self.state.calls.lock().unwrap().push(request.clone());
        let arguments = request.arguments();
        match arguments.first().map(String::as_str) {
            Some("info") => ok(PARAKEET_INFO),
            Some("run") if arguments.get(2).map(String::as_str) == Some("version") => {
                if !self.state.status_started.swap(true, Ordering::SeqCst) {
                    poll_fn(|context| {
                        if self.state.release_status.load(Ordering::SeqCst) {
                            Poll::Ready(())
                        } else {
                            *self.state.status_waker.lock().unwrap() =
                                Some(context.waker().clone());
                            Poll::Pending
                        }
                    })
                    .await;
                }
                ok(VERSION)
            }
            Some("run") if arguments.get(2).map(String::as_str) == Some("status") => ok(STATUS),
            Some("run") if arguments.get(2).map(String::as_str) == Some("get") => ok(GET),
            Some("run") if arguments.get(2).map(String::as_str) == Some("list-models") => {
                ok(MODELS)
            }
            Some("run") if arguments.get(2).map(String::as_str) == Some("list-engines") => {
                ok(ENGINES)
            }
            _ => Err(CommandError::FakeScriptExhausted),
        }
    }
}

#[test]
fn refresh_prevents_an_in_flight_resolver_from_repopulating_the_cache() {
    let runner = RefreshRaceRunner::new();
    let repository =
        SnapBackendRepository::new(Arc::new(runner.clone()), Arc::new(FakeSnapd::default()));
    let backend = BackendIdentity::new("myna-parakeet", "provider");

    block_on(async {
        let mut in_flight = Box::pin(repository.read_snapshot(&backend, CancellationToken::new()));
        poll_fn(|context| match in_flight.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("modelctl verification should still be in flight"),
        })
        .await;
        assert!(runner.state.status_started.load(Ordering::SeqCst));

        repository.refresh(CancellationToken::new()).await.unwrap();
        runner.release_status();
        assert!(in_flight.await.errors().is_empty());
        assert!(repository
            .read_snapshot(&backend, CancellationToken::new())
            .await
            .errors()
            .is_empty());
    });

    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| call.arguments().first().map(String::as_str) == Some("info"))
            .count(),
        2
    );
}

#[test]
fn every_read_surface_can_fail_without_discarding_the_others() {
    let cases = [
        (2, BackendSurface::Status),
        (3, BackendSurface::ModelctlConfig),
        (4, BackendSurface::Models),
        (5, BackendSurface::Engines),
    ];

    for (failed_index, expected_surface) in cases {
        let mut outcomes = vec![
            ok(PARAKEET_INFO),
            ok(VERSION),
            ok(STATUS),
            ok(GET),
            ok(MODELS),
            ok(ENGINES),
        ];
        outcomes[failed_index] = failed("independent surface");
        let (repository, runner) = repository(outcomes);

        let snapshot = block_on(repository.read_snapshot(
            &BackendIdentity::new("myna-parakeet", "provider"),
            CancellationToken::new(),
        ));

        assert!(snapshot.error(expected_surface).is_some());
        assert!(runner.calls().len() <= 6);
        assert_eq!(
            snapshot.errors().len(),
            1,
            "only {expected_surface:?} should fail"
        );
    }
}

#[test]
fn provider_role_discovery_expands_snap_data_and_rejects_escaping_paths() {
    let dir = std::env::temp_dir().join(format!("myna-config-role-edge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("custom-speech").join("current/share");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("provider.env"), "MYNA_ROLE=speech\n").unwrap();
    let bare_common = dir.join("bare-speech").join("common");
    std::fs::create_dir_all(&bare_common).unwrap();
    std::fs::write(bare_common.join("provider.env"), "MYNA_ROLE=speech\n").unwrap();
    // Outside snap dir: must never be read via ../ traversal.
    std::fs::write(dir.join("provider.env"), "MYNA_ROLE=speech\n").unwrap();

    let connections: ContentConnections = serde_json::from_value(serde_json::json!({
        "slots": [
            {
                "snap": "custom-speech",
                "slot": "provider",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "read": ["$SNAP_DATA/share"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            },
            {
                "snap": "bare-speech",
                "slot": "provider",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "write": ["$SNAP_COMMON"] }
                },
                "connections": []
            },
            {
                "snap": "escaping-slot",
                "slot": "provider",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "write": ["$SNAP_COMMON/../..", "/etc"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            },
            {
                "snap": "../bad-snap",
                "slot": "provider",
                "attrs": {
                    "content": "inference-provider",
                    "source": { "write": ["$SNAP_COMMON"] }
                },
                "connections": [{ "snap": "myna", "plug": "backend" }]
            }
        ]
    }))
    .unwrap();

    let runner = FakeCommandRunner::default();
    let snapd = Arc::new(FakeSnapd::answering(Ok(Vec::new()), Ok(connections)));
    let repository = SnapBackendRepository::with_snap_root(Arc::new(runner), snapd, &dir);
    let discovered = block_on(repository.discover(CancellationToken::new())).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        discovered.backends(),
        &[
            BackendIdentity::new("bare-speech", "provider"),
            BackendIdentity::new("custom-speech", "provider"),
        ]
    );
    assert_eq!(
        discovered.text_providers(),
        &[
            BackendIdentity::new("../bad-snap", "provider"),
            BackendIdentity::new("escaping-slot", "provider"),
        ]
    );
}
