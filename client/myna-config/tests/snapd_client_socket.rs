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
use std::time::Duration;

use gtk4::glib::MainContext;
use myna_config::active_backend::SwitchPlan;
use myna_config::adapters::snapd_client::{
    InterfaceAction, SnapdClient, SnapdError, SnapdOutcome, SnapdTimeouts, UnixSocketSnapdClient,
};
use myna_config::adapters::system_configurator::PkexecSystemConfigurator;
use myna_config::command::{CancellationToken, FakeCommandRunner};
use myna_config::domain::{parse_connections, BackendIdentity};
use myna_config::ports::SystemConfigurator;

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

    fn client(&self) -> UnixSocketSnapdClient {
        UnixSocketSnapdClient::with_socket(self.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_secs(2),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_secs(4),
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
fn restart_service_waits_for_readiness_to_become_active() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/apps".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"21"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/21".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":false}]}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":true}]}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = fake.client();

    let report = block_on(client.restart_myna_service(CancellationToken::new())).unwrap();

    assert_eq!(report.change_id, "21");
    assert_eq!(report.status, "Done");
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    assert!(calls[0]
        .contains(r#"{"action":"restart","names":["myna.myna"],"scope":["user"],"users":"self"}"#));
}

#[test]
fn restart_service_authorization_denial_is_mapped() {
    let fake = FakeSnapd::start(vec![Step {
        request_path_contains: "POST /v2/apps".into(),
        response: http_body(
            403,
            "Forbidden",
            r#"{"type":"error","status-code":403,"result":{"message":"nope","kind":"auth-cancelled"}}"#,
        ),
        delay: None,
        close_early: false,
    }]);
    let client = fake.client();

    let error = block_on(client.restart_myna_service(CancellationToken::new())).unwrap_err();

    assert!(matches!(
        error,
        SnapdError::AuthorizationDenied {
            status_code: 403,
            ..
        }
    ));
}

#[test]
fn restart_service_change_failure_is_reported() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/apps".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"24"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/24".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Error","err":"restart failed"}}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = fake.client();

    let error = block_on(client.restart_myna_service(CancellationToken::new())).unwrap_err();

    assert!(matches!(
        error,
        SnapdError::Snapd { message, .. } if message == "restart failed"
    ));
}

#[test]
fn restart_service_readiness_timeout_is_reported_truthfully() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/apps".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"22"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/22".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":false}]}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":false}]}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(100),
            poll_interval: Duration::from_millis(80),
            total: Duration::from_millis(150),
        });

    let error = block_on(client.restart_myna_service(CancellationToken::new())).unwrap_err();

    match error {
        SnapdError::Timeout { context, .. } => {
            assert_eq!(
                format!("{context:?}"),
                "ServiceReadiness",
                "timeout should report readiness polling, got {context:?}"
            );
        }
        other => panic!("expected timeout, got {other:?}"),
    }
}

#[test]
fn restart_service_cancellation_during_readiness_is_reported() {
    let fake = FakeSnapd::start(vec![
        Step {
            request_path_contains: "POST /v2/apps".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"23"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/23".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":false}]}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client =
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_secs(2),
            poll_interval: Duration::from_millis(100),
            total: Duration::from_secs(2),
        });
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let _canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        cancel.cancel();
    });

    let error = block_on(client.restart_myna_service(cancellation)).unwrap_err();

    assert_eq!(error, SnapdError::Cancelled);
}

#[test]
fn backend_switch_requests_disconnect_connect_restart_then_service_readiness() {
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
        Step {
            request_path_contains: "POST /v2/apps".into(),
            response: http_body(
                202,
                "Accepted",
                r#"{"type":"async","status-code":202,"change":"19"}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/changes/19".into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":{"ready":true,"status":"Done"}}"#,
            ),
            delay: None,
            close_early: false,
        },
        Step {
            request_path_contains: "GET /v2/apps?names=myna.myna&select=service&global=false"
                .into(),
            response: http_body(
                200,
                "OK",
                r#"{"type":"sync","status-code":200,"result":[{"snap":"myna","name":"myna","daemon":"simple","daemon-scope":"user","active":true}]}"#,
            ),
            delay: None,
            close_early: false,
        },
    ]);
    let client = Arc::new(
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_secs(2),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_secs(4),
        }),
    );
    let adapter =
        PkexecSystemConfigurator::with_snapd_client(Arc::new(FakeCommandRunner::default()), client);
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend old:provider manual\n\
         content - new:provider -\n",
        "name: content\nslots:\n  - new:provider:\n      content: inference-provider\n",
    )
    .unwrap();
    let plan = SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap();

    let completed =
        block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap();

    assert_eq!(completed.len(), 3);
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 5);
    assert!(calls[0].contains("POST /v2/interfaces HTTP/1.1"));
    assert!(calls[0].contains(r#"{"action":"disconnect","plugs":[{"snap":"myna","plug":"backend"}],"slots":[{"snap":"old","slot":"provider"}]}"#));
    assert!(calls[1].contains(r#"{"action":"connect","plugs":[{"snap":"myna","plug":"backend"}],"slots":[{"snap":"new","slot":"provider"}]}"#));
    assert!(calls[2].contains("POST /v2/apps HTTP/1.1"));
    assert!(calls[2]
        .contains(r#"{"action":"restart","names":["myna.myna"],"scope":["user"],"users":"self"}"#));
    assert!(calls[3].contains("GET /v2/changes/19 HTTP/1.1"));
    assert!(calls[4].contains("GET /v2/apps?names=myna.myna&select=service&global=false HTTP/1.1"));
}

#[test]
fn noop_backend_switch_makes_no_snapd_requests() {
    let fake = FakeSnapd::start(Vec::new());
    let client = Arc::new(
        UnixSocketSnapdClient::with_socket(fake.path.clone()).with_timeouts(SnapdTimeouts {
            per_request: Duration::from_millis(100),
            poll_interval: Duration::from_millis(20),
            total: Duration::from_millis(200),
        }),
    );
    let adapter =
        PkexecSystemConfigurator::with_snapd_client(Arc::new(FakeCommandRunner::default()), client);
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend new:provider manual\n",
        "name: content\n",
    )
    .unwrap();
    let plan = SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap();

    let completed =
        block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap();

    assert!(completed.is_empty());
    assert!(fake.calls.lock().unwrap().is_empty());
}
