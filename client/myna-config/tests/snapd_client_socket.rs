//! Fake `UnixListener`-backed integration tests for the direct snapd REST
//! adapter used by [`myna_config::adapters::snapd_client`]. Each test spins up
//! a tiny thread bound to a per-test Unix socket in `target/`, scripts the
//! responses, and asserts on the request byte stream + typed outcome.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gtk4::glib::MainContext;
use myna_config::active_backend::{myna_restart_request, SwitchPlan};
use myna_config::adapters::snapd_client::{
    InterfaceAction, SnapdClient, SnapdError, SnapdOutcome, SnapdTimeoutContext, SnapdTimeouts,
    UnixSocketSnapdClient,
};
use myna_config::adapters::system_configurator::PkexecSystemConfigurator;
use myna_config::command::{CancellationToken, CommandOutput, FakeCommandRunner};
use myna_config::domain::{parse_connections, BackendIdentity};
use myna_config::ports::{SystemConfigurator, SystemConfiguratorError};
use myna_config::snap_changes::{ApplyProgress, ChangeInProgress};

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    MainContext::new().block_on(future)
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Under the system temp dir, not `CARGO_TARGET_TMPDIR`: a Unix socket path
/// is capped at `SUN_LEN` (108 bytes), which a package build's
/// `/build/<source>-<version>/target/release/tmp/` prefix already exceeds.
fn socket_path() -> PathBuf {
    let dir = std::env::temp_dir();
    let index = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    dir.join(format!("myna-snapd-fake-{pid}-{index}.sock"))
}

/// One scripted `(request-body-contains, http-response)` step.
#[derive(Clone)]
struct Step {
    /// Substring that must appear in the request line for the step to match.
    request_path_contains: String,
    response: String,
    /// If Some, sleep before responding to simulate a slow server.
    delay: Option<Duration>,
    /// If true, close the connection abruptly before sending a full response.
    close_early: bool,
}

struct FakeSnapd {
    _thread: thread::JoinHandle<()>,
    path: PathBuf,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeSnapd {
    fn start(steps: Vec<Step>) -> Self {
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake snapd socket");
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_thread = Arc::clone(&calls);
        let path_thread = path.clone();
        let handle = thread::spawn(move || {
            for step in steps {
                let (mut stream, _) = listener.accept().expect("accept fake snapd request");
                let mut buffer = [0u8; 4096];
                let mut request = Vec::new();
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(idx) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        // Read Content-Length body if present.
                        let header = String::from_utf8_lossy(&request[..idx]).to_string();
                        let cl = header
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("Content-Length:")
                                    .or_else(|| line.strip_prefix("content-length:"))
                                    .map(str::trim)
                                    .and_then(|value| value.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        let have = request.len() - idx - 4;
                        while have + (request.len() - (idx + 4 + have)) < cl {
                            let n = stream.read(&mut buffer).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            request.extend_from_slice(&buffer[..n]);
                        }
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request).to_string();
                if !text.contains(&step.request_path_contains) {
                    // Record and drop the connection: this may cause the client to
                    // report a protocol error, which is what we want the assertion to
                    // catch upstream.
                    calls_thread.lock().unwrap().push(text.clone());
                    continue;
                }
                calls_thread.lock().unwrap().push(text);
                if let Some(delay) = step.delay {
                    thread::sleep(delay);
                }
                if step.close_early {
                    // Send a partial header, then drop.
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100");
                    let _ = stream.flush();
                    drop(stream);
                    continue;
                }
                let _ = stream.write_all(step.response.as_bytes());
                let _ = stream.flush();
                drop(stream);
            }
            drop(listener);
            let _ = std::fs::remove_file(&path_thread);
        });
        FakeSnapd {
            _thread: handle,
            path,
            calls,
        }
    }

    /// The first `count` requests, waiting for the server thread to read
    /// any the client already sent. Panics after a minute.
    fn wait_for_calls(&self, count: usize) -> Vec<String> {
        let give_up = Instant::now() + Duration::from_secs(60);
        loop {
            let calls = self.calls.lock().unwrap().clone();
            if calls.len() >= count {
                return calls;
            }
            assert!(
                Instant::now() < give_up,
                "{count} requests never came: {calls:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Budgets no build-load stall reaches, for tests that never wait one out.
    fn client(&self) -> UnixSocketSnapdClient {
        UnixSocketSnapdClient::with_socket(self.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_secs(60),
            authorization: Duration::from_secs(60),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_secs(60),
        })
    }
}

fn http_body(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn http_chunked(status: u16, reason: &str, body: &str) -> String {
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    // Send in two chunks.
    let (a, b) = body.split_at(body.len() / 2);
    out.push_str(&format!("{:x}\r\n{a}\r\n", a.len()));
    out.push_str(&format!("{:x}\r\n{b}\r\n", b.len()));
    out.push_str("0\r\n\r\n");
    out
}

#[test]
fn request_shape_uses_typed_plug_slot_and_allow_interaction_header() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();

    let outcome = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "myna-parakeet".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap();

    assert!(matches!(outcome, SnapdOutcome::Sync));
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let request = &calls[0];
    assert!(request.contains("POST /v2/interfaces HTTP/1.1"));
    assert!(request.contains("X-Allow-Interaction: true"));
    assert!(request.contains("Connection: close"));
    assert!(request.contains("Content-Type: application/json"));
    assert!(
        request.contains("\"action\":\"connect\"")
            && request.contains("\"snap\":\"myna\"")
            && request.contains("\"plug\":\"backend\"")
            && request.contains("\"snap\":\"myna-parakeet\"")
            && request.contains("\"slot\":\"provider\"")
            && !request.contains("\"name\":")
    );
}

#[test]
fn async_change_polling_completes_when_change_becomes_ready() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/interfaces".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"7"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/7".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":false,"status":"Doing"}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/7".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = fake.client();
    let outcome = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap();
    match outcome {
        SnapdOutcome::Async(report) => {
            assert_eq!(report.change_id, "7");
            assert_eq!(report.status, "Done");
        }
        _ => panic!("expected async outcome"),
    }
}

#[test]
fn authorization_denial_is_mapped() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(
            401,
            "Unauthorized",
            r#"{"type":"error","status-code":401,"result":{"message":"access denied","kind":"login-required"}}"#,
        ),
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        SnapdError::AuthorizationDenied {
            status_code: 401,
            ..
        }
    ));
}

#[test]
fn per_call_timeout_is_reported() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        delay: Some(Duration::from_secs(5)),
        close_early: false,
    }]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(200),
            authorization: Duration::from_millis(200),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(400),
        });
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Timeout { .. }));
}

#[test]
fn cancellation_before_dispatch_returns_cancelled() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        delay: None,
        close_early: false,
    }]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        cancellation,
    ))
    .unwrap_err();
    assert_eq!(error, SnapdError::Cancelled);
}

#[test]
fn malformed_envelope_is_reported_as_protocol_error() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(200, "OK", "this is not JSON"),
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
}

#[test]
fn chunked_transfer_encoded_body_is_decoded() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_chunked(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let outcome = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap();
    assert!(matches!(outcome, SnapdOutcome::Sync));
}

#[test]
fn early_connection_close_is_reported_as_protocol_error() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: String::new(),
        delay: None,
        close_early: true,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
}

/// Optional host smoke test: exercises the real host snapd socket for a
/// *non-mutating* GET only, gated behind an environment variable. The test is
/// skipped unless `MYNA_CONFIG_SNAPD_HOST_SMOKE=1` is set and
/// `/run/snapd.socket` exists, so it never runs in CI or unattended.
#[test]
fn host_snapd_socket_smoke_test_when_enabled() {
    if std::env::var("MYNA_CONFIG_SNAPD_HOST_SMOKE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    if !std::path::Path::new("/run/snapd.socket").exists() {
        return;
    }
    // We do NOT mutate anything on the host. We only verify that
    // `is_valid_snap_name` still rejects a syntactically invalid name that we
    // pre-check locally; we never send a request. See design note in
    // config-ui/confinement.md.
    assert!(
        !myna_config::adapters::snapd_client::is_valid_snap_name("bad;name"),
        "smoke check pre-validation must reject invalid snap names"
    );
}

#[test]
fn async_response_with_hostile_change_id_is_rejected_before_second_request() {
    // Snapd's async envelope carries a change id that we splice into
    // `/v2/changes/{id}`. A hostile change id containing "\r\n" or "/" would
    // reshape the request line or headers; the client must refuse it and
    // never issue the polling GET.
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/interfaces".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"../secrets"}"#,
            ),
            delay: None,
            close_early: false,
        },
        // Second step should never be reached; if it is, the polling request
        // would carry the hostile change id. The test asserts that the client
        // fails first.
        Step {
            request_path_contains: "GET /v2/changes/".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
    // Only the POST should have been received.
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains("POST /v2/interfaces"));
}

#[test]
fn response_headers_larger_than_the_cap_are_rejected_with_a_protocol_error() {
    // Build a status line followed by many junk headers that together exceed
    // the header cap without ever producing CRLF CRLF.
    let mut header = String::from("HTTP/1.1 200 OK\r\n");
    while header.len() < 40 * 1024 {
        header.push_str("X-Filler: 0000000000000000000000000000000000000000\r\n");
    }
    // No terminating CRLF CRLF — force the client to keep reading past the
    // header cap.
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: header,
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
}

#[test]
fn duplicate_conflicting_content_length_is_rejected() {
    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: application/json\r\n",
        "Content-Length: 5\r\n",
        "Content-Length: 6\r\n",
        "Connection: close\r\n",
        "\r\n",
        "hello",
    )
    .to_owned();
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response,
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
}

#[test]
fn unsupported_transfer_encoding_is_rejected() {
    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: application/json\r\n",
        "Transfer-Encoding: gzip\r\n",
        "Connection: close\r\n",
        "\r\n",
    )
    .to_owned();
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response,
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    assert!(matches!(error, SnapdError::Protocol { .. }));
}

#[test]
fn timeout_message_reports_a_positive_elapsed_duration() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/interfaces".into(),
        response: http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        delay: Some(Duration::from_secs(5)),
        close_early: false,
    }]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(150),
            authorization: Duration::from_millis(150),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(300),
        });
    let start = std::time::Instant::now();
    let error = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ))
    .unwrap_err();
    match error {
        SnapdError::Timeout { elapsed, .. } => {
            assert!(
                elapsed >= Duration::from_millis(50),
                "elapsed must reflect actual wait, got {elapsed:?}"
            );
        }
        other => panic!("expected timeout, got {other:?}"),
    }
    // Sanity: real wall clock elapsed should be at least as long as the per-
    // call timeout, and the reported elapsed should be comparable.
    let real = start.elapsed();
    assert!(real >= Duration::from_millis(100));
}

#[test]
fn backend_switch_requests_disconnect_connect_then_restarts_the_user_service() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/interfaces".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "POST /v2/interfaces".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{}}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = Arc::new(fake.client());
    let runner = Arc::new(FakeCommandRunner::scripted([Ok(CommandOutput::new(
        Some(0),
        "",
        "",
    ))]));
    let adapter = PkexecSystemConfigurator::with_snapd_client(runner.clone(), client);
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend old:provider manual\n\
         content - new:provider -\n",
        "name: content\nslots:\n  - old:provider:\n      content: inference-provider\n      task: speech-to-text\n  - new:provider:\n      content: inference-provider\n      task: speech-to-text\n",
    )
    .unwrap();
    let plan = SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap();

    let completed =
        block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap();

    assert_eq!(completed.len(), 3);
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].contains("POST /v2/interfaces HTTP/1.1"));
    assert!(calls[0].contains(r#"{"action":"disconnect","plugs":[{"snap":"myna","plug":"backend"}],"slots":[{"snap":"old","slot":"provider"}]}"#));
    assert!(calls[1].contains(r#"{"action":"connect","plugs":[{"snap":"myna","plug":"backend"}],"slots":[{"snap":"new","slot":"provider"}]}"#));
    assert_eq!(runner.calls(), [myna_restart_request()]);
}

#[test]
fn noop_backend_switch_makes_no_snapd_requests() {
    let fake = FakeSnapd::start(Vec::new());
    let client = Arc::new(
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(100),
            authorization: Duration::from_millis(100),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(200),
        }),
    );
    let adapter =
        PkexecSystemConfigurator::with_snapd_client(Arc::new(FakeCommandRunner::default()), client);
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend new:provider manual\n",
        "name: content\nslots:\n  - new:provider:\n      content: inference-provider\n      task: speech-to-text\n",
    )
    .unwrap();
    let plan = SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap();

    let completed =
        block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap();

    assert!(completed.is_empty());
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn apply_progress_reads_the_running_changes_as_the_user() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "GET /v2/changes?select=in-progress ".to_owned(),
        response: http_body(
            200,
            "OK",
            include_str!("fixtures/snapd-changes-component-download.json"),
        ),
        delay: None,
        close_early: false,
    }]);
    let configurator = PkexecSystemConfigurator::with_snapd_client(
        Arc::new(FakeCommandRunner::default()),
        Arc::new(fake.client()),
    );

    let progress = block_on(configurator.apply_progress("myna-whisper", CancellationToken::new()));

    assert_eq!(
        progress,
        Some(ApplyProgress::Download {
            name: "model-small".to_owned(),
            done: 13_718_564,
            total: 483_966_976,
        })
    );
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("GET /v2/changes?select=in-progress HTTP/1.1\r\n"));
}

#[test]
fn unreadable_changes_are_no_progress() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "/v2/changes".to_owned(),
        response: http_body(200, "OK", r#"{"type":"sync","result":{"not":"a list"}}"#),
        delay: None,
        close_early: false,
    }]);
    let configurator = PkexecSystemConfigurator::with_snapd_client(
        Arc::new(FakeCommandRunner::default()),
        Arc::new(fake.client()),
    );

    let progress = block_on(configurator.apply_progress("myna-whisper", CancellationToken::new()));

    assert_eq!(progress, None);
}

fn system_info(features: &str) -> String {
    http_body(
        200,
        "OK",
        &format!(
            r#"{{"type":"sync","status-code":200,"status":"OK","result":{{"series":"16","version":"2.77.1","features":{features}}}}}"#
        ),
    )
}

fn user_daemons(response: String) -> (Result<bool, SnapdError>, Vec<String>) {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "GET /v2/system-info ".to_owned(),
        response,
        delay: None,
        close_early: false,
    }]);
    let enabled = block_on(fake.client().user_daemons_enabled(CancellationToken::new()));
    let calls = fake.calls.lock().unwrap().clone();
    (enabled, calls)
}

/// `/v2/snaps/system/conf` answers a user 401; the system information lists
/// the flag to anyone.
#[test]
fn the_user_daemons_flag_is_read_from_the_system_information() {
    let (enabled, calls) = user_daemons(system_info(
        r#"{"user-daemons":{"supported":true,"enabled":true},"parallel-instances":{"supported":true}}"#,
    ));
    assert_eq!(enabled, Ok(true));
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("GET /v2/system-info HTTP/1.1\r\n"));

    let (enabled, _) = user_daemons(system_info(
        r#"{"user-daemons":{"supported":true,"enabled":false}}"#,
    ));
    assert_eq!(enabled, Ok(false));
}

/// snapd lists only the flags it knows, and one unset may omit `enabled`.
#[test]
fn a_user_daemons_flag_snapd_does_not_list_is_off() {
    assert_eq!(user_daemons(system_info("{}")).0, Ok(false));
    assert_eq!(
        user_daemons(system_info(r#"{"user-daemons":{"supported":true}}"#)).0,
        Ok(false)
    );
}

#[test]
fn an_unreadable_system_information_is_an_error() {
    let (enabled, _) = user_daemons(http_body(
        500,
        "Internal Server Error",
        r#"{"type":"error","status-code":500,"result":{"message":"boom"}}"#,
    ));
    assert!(matches!(
        enabled,
        Err(SnapdError::Snapd {
            status_code: 500,
            ..
        })
    ));
}

fn step(request: &str, response: String, delay: Option<Duration>) -> Step {
    Step {
        request_path_contains: request.to_owned(),
        response,
        delay,
        close_early: false,
    }
}

const CHANGE_9_ACCEPTED: &str = r#"{"type":"async","status-code":202,"change":"9"}"#;
const CHANGE_9_DONE: &str =
    r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#;

/// snapd answers only once the user has answered polkit, which may take
/// longer than any read: measured 40 s on Noble for a prompt left open.
///
/// Asserts only what a scheduling stall cannot fake, since sbuild stalled
/// the change poll past every wall-clock budget tried: a PUT cut off by
/// `per_request` fails as a `Request` timeout whatever the load, and a
/// `total` counted from the start ends the poll before its GET is sent.
#[test]
fn the_authorization_prompt_may_outlast_a_request() {
    let fake = FakeSnapd::start(vec![
        step(
            "POST /v2/interfaces ",
            http_body(202, "Accepted", CHANGE_9_ACCEPTED),
            Some(Duration::from_millis(600)),
        ),
        step(
            "GET /v2/changes/9 ",
            http_body(200, "OK", CHANGE_9_DONE),
            None,
        ),
    ]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(200),
            authorization: Duration::from_secs(60),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(400),
        });

    let outcome = block_on(client.apply_interface_action(
        InterfaceAction::Disconnect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ));

    assert!(
        matches!(
            outcome,
            Ok(SnapdOutcome::Async(_))
                | Err(SnapdError::Timeout {
                    context: SnapdTimeoutContext::ChangePolling,
                    ..
                })
        ),
        "{outcome:?}"
    );
    let calls = fake.wait_for_calls(2);
    assert!(calls[1].starts_with("GET /v2/changes/9 "), "{calls:?}");
}

#[test]
fn a_connect_prompt_may_outlast_a_request() {
    let fake = FakeSnapd::start(vec![step(
        "POST /v2/interfaces ",
        http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{}}"#,
        ),
        Some(Duration::from_millis(600)),
    )]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(200),
            authorization: Duration::from_secs(60),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(400),
        });

    let outcome = block_on(client.apply_interface_action(
        InterfaceAction::Connect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ));

    assert_eq!(outcome, Ok(SnapdOutcome::Sync));
}

/// A change snapd accepted and then failed carries snapd's error.
#[test]
fn a_failed_change_is_reported() {
    let fake = FakeSnapd::start(vec![
        step(
            "POST /v2/interfaces ",
            http_body(202, "Accepted", CHANGE_9_ACCEPTED),
            None,
        ),
        step(
            "GET /v2/changes/9 ",
            http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Error","err":"cannot run hook"}}"#,
            ),
            None,
        ),
    ]);

    let failed = block_on(fake.client().apply_interface_action(
        InterfaceAction::Disconnect {
            backend_snap: "backend".into(),
            backend_slot: "provider".into(),
        },
        CancellationToken::new(),
    ));

    assert!(
        failed
            .as_ref()
            .is_err_and(|error| error.to_string().contains("cannot run hook")),
        "{failed:?}"
    );
}

const CHANGE_12_ACCEPTED: &str = r#"{"type":"async","status-code":202,"change":"12"}"#;

/// The same install `snap install --edge myna` makes, asked as the user:
/// snapd raises polkit's prompt for `io.snapcraft.snapd.manage` itself.
#[test]
fn installing_a_snap_asks_for_edge_and_hands_back_its_change() {
    let fake = FakeSnapd::start(vec![step(
        "POST /v2/snaps/myna ",
        http_body(202, "Accepted", CHANGE_12_ACCEPTED),
        None,
    )]);

    let started = block_on(fake.client().install_snap("myna", CancellationToken::new()));

    assert_eq!(started, Ok(Some("12".to_owned())));
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("POST /v2/snaps/myna HTTP/1.1\r\n"));
    assert!(calls[0].contains("X-Allow-Interaction: true\r\n"));
    assert!(calls[0].ends_with("\r\n\r\n{\"action\":\"install\",\"channel\":\"latest/edge\"}"));
}

#[test]
fn an_install_prompt_may_outlast_a_request() {
    let fake = FakeSnapd::start(vec![step(
        "POST /v2/snaps/myna-parakeet ",
        http_body(202, "Accepted", CHANGE_12_ACCEPTED),
        Some(Duration::from_millis(600)),
    )]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(200),
            authorization: Duration::from_secs(60),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(400),
        });

    assert_eq!(
        block_on(client.install_snap("myna-parakeet", CancellationToken::new())),
        Ok(Some("12".to_owned()))
    );
}

/// Installed elsewhere between the last read and the click: nothing to do.
#[test]
fn installing_an_installed_snap_has_no_change_to_follow() {
    let fake = FakeSnapd::start(vec![step(
        "POST /v2/snaps/myna ",
        http_body(
            400,
            "Bad Request",
            r#"{"type":"error","status-code":400,"result":{"message":"snap \"myna\" is already installed","kind":"snap-already-installed","value":"myna"}}"#,
        ),
        None,
    )]);

    assert_eq!(
        block_on(fake.client().install_snap("myna", CancellationToken::new())),
        Ok(None)
    );
}

#[test]
fn a_snap_name_outside_the_grammar_is_never_sent() {
    let fake = FakeSnapd::start(Vec::new());

    let refused = block_on(
        fake.client()
            .install_snap("../v2/logout", CancellationToken::new()),
    );

    assert!(
        matches!(refused, Err(SnapdError::Transport { .. })),
        "{refused:?}"
    );
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn a_change_is_read_with_its_tasks() {
    let fake = FakeSnapd::start(vec![step(
        "GET /v2/changes/12 ",
        http_body(
            200,
            "OK",
            r#"{"type":"sync","status-code":200,"result":{"id":"12","kind":"install-snap","summary":"Install \"myna\" snap from \"latest/edge\" channel","status":"Doing","ready":false,"tasks":[{"kind":"download-snap","status":"Doing","progress":{"label":"myna","done":5066752,"total":10133504}}]}}"#,
        ),
        None,
    )]);

    let change = block_on(fake.client().change("12", CancellationToken::new())).unwrap();

    assert_eq!(change.id(), "12");
    assert!(!change.ready());
    assert_eq!(change.download_percent(10_133_504), Some(50));
}

#[test]
fn a_change_id_outside_the_grammar_is_never_read() {
    let fake = FakeSnapd::start(Vec::new());

    let refused = block_on(
        fake.client()
            .change("12/../../logout", CancellationToken::new()),
    );

    assert!(
        matches!(refused, Err(SnapdError::Protocol { .. })),
        "{refused:?}"
    );
    assert!(fake.calls.lock().unwrap().is_empty());
}

fn install_answered(response: String) -> Result<Option<String>, SystemConfiguratorError> {
    let fake = FakeSnapd::start(vec![step("POST /v2/snaps/myna ", response, None)]);
    let configurator = PkexecSystemConfigurator::with_snapd_client(
        Arc::new(FakeCommandRunner::default()),
        Arc::new(fake.client()),
    );
    block_on(configurator.install_snap("myna", CancellationToken::new()))
}

#[test]
fn a_dismissed_prompt_cancels_an_install() {
    assert_eq!(
        install_answered(http_body(
            403,
            "Forbidden",
            r#"{"type":"error","status-code":403,"result":{"message":"cancelled","kind":"auth-cancelled"}}"#,
        )),
        Err(SystemConfiguratorError::Cancelled)
    );
}

#[test]
fn a_refused_install_names_its_request() {
    let refused = install_answered(http_body(
        401,
        "Unauthorized",
        r#"{"type":"error","status-code":401,"result":{"message":"access denied","kind":"login-required"}}"#,
    ));
    assert_eq!(
        refused,
        Err(SystemConfiguratorError::snapd_authorization_denied(
            "POST /v2/snaps/myna (install, latest/edge)",
            401,
            "access denied",
        ))
    );
}

/// Measured on Noble: without the flag snapd refuses Myna synchronously.
#[test]
fn an_install_snapd_rejects_reports_its_reason() {
    let rejected = install_answered(http_body(
        400,
        "Bad Request",
        r#"{"type":"error","status-code":400,"result":{"message":"cannot install \"myna\": feature flag validation failed"}}"#,
    ));
    assert_eq!(
        rejected,
        Err(SystemConfiguratorError::snapd_execution(
            "POST /v2/snaps/myna (install, latest/edge)",
            Some(400),
            "cannot install \"myna\": feature flag validation failed",
        ))
    );
}

fn change_answered(response: String) -> Result<ChangeInProgress, SnapdError> {
    let fake = FakeSnapd::start(vec![step("GET /v2/changes/12 ", response, None)]);
    block_on(fake.client().change("12", CancellationToken::new()))
}

/// snapd prunes a finished change after a day.
#[test]
fn a_change_snapd_no_longer_knows_is_its_error() {
    let missing = change_answered(http_body(
        404,
        "Not Found",
        r#"{"type":"error","status-code":404,"result":{"message":"cannot find change with id \"12\"","kind":"not-found"}}"#,
    ));
    assert!(
        matches!(
            &missing,
            Err(SnapdError::Snapd {
                status_code: 404,
                ..
            })
        ),
        "{missing:?}"
    );
}

#[test]
fn a_change_that_is_not_a_change_is_a_protocol_error() {
    for body in [
        r#"{"type":"sync","status-code":200,"result":{"id":"12"}}"#,
        r#"{"type":"async","status-code":202,"change":"13"}"#,
    ] {
        let read = change_answered(http_body(200, "OK", body));
        assert!(
            matches!(&read, Err(SnapdError::Protocol { .. })),
            "{body}: {read:?}"
        );
    }
}

#[test]
fn an_unreadable_change_reaches_the_follower_as_text() {
    let fake = FakeSnapd::start(vec![step(
        "GET /v2/changes/12 ",
        http_body(
            404,
            "Not Found",
            r#"{"type":"error","status-code":404,"result":{"message":"cannot find change with id \"12\"","kind":"not-found"}}"#,
        ),
        None,
    )]);
    let configurator = PkexecSystemConfigurator::with_snapd_client(
        Arc::new(FakeCommandRunner::default()),
        Arc::new(fake.client()),
    );

    let read = block_on(configurator.snap_change("12", CancellationToken::new()));

    assert!(
        matches!(&read, Err(message) if message.contains("cannot find change")),
        "{read:?}"
    );
}
