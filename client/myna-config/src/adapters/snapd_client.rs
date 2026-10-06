//! Direct host snapd REST adapter for backend-switch interface operations.
//!
//! This module speaks HTTP/1.1 over the abstract-free Unix domain socket at
//! `/run/snapd.socket` and only exposes a narrowly typed API for the exact
//! operations required by the active-backend switch:
//!
//! * `POST /v2/interfaces` with a body of the form
//!   `{"action": "connect"|"disconnect", "plugs":[{"snap":"myna","plug":"backend"}],
//!    "slots":[{"snap":<backend>,"slot":<slot>}]}` where `<backend>` is a
//!   validated snap name and `<slot>` the validated slot name discovery
//!   recorded for it.
//! * `GET  /v2/changes/{id}` to poll async changes to completion.
//! * `GET  /v2/changes?select=in-progress` to show what an apply waits on.
//! * `GET  /v2/system-info` to read whether `experimental.user-daemons` is on.
//! * `POST /v2/snaps/{name}` with exactly
//!   `{"action":"install","channel":"latest/edge"}`, `{name}` a validated
//!   snap name, to install it as `snap install --edge` does.
//!
//! There is deliberately no way to send an arbitrary path, method, body, or
//! header through this module. Snap names are re-validated against the strict
//! snap-name grammar. The transport sends `X-Allow-Interaction: true` so that
//! snapd can request administrator authorization via polkit when required, and
//! `Connection: close` so that the socket is single-request and the server
//! signals response end cleanly.
//!
//! All blocking Unix-socket I/O runs off the GTK main loop via
//! [`gio::spawn_blocking`] so the UI thread never blocks.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::command::CancellationToken;
use crate::snap_changes::{parse_change, parse_changes, ChangeInProgress};

/// Every change snapd has not finished. Not `for=<snap>`: snapd cannot name
/// the snap of the `snapctl-install` change a model download runs in.
const CHANGES_IN_PROGRESS: &str = "/v2/changes?select=in-progress";

/// Unlike `/v2/snaps/system/conf`, which answers a user 401, this lists the
/// experimental flags to anyone.
const SYSTEM_INFO: &str = "/v2/system-info";

pub(crate) const INTERFACES: &str = "/v2/interfaces";

const INSTALL_ON_EDGE: &str = r#"{"action":"install","channel":"latest/edge"}"#;

/// Default host location of the snapd socket. The value is deliberately a
/// constant, not user configurable, so an attacker cannot redirect the client.
pub const DEFAULT_SNAPD_SOCKET: &str = "/run/snapd.socket";

/// Maximum HTTP response size we will read from snapd. 1 MiB is far larger
/// than any legitimate `/v2/interfaces` or `/v2/changes/{id}` response.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Independent bound on the HTTP header block. Snapd headers are tiny; this
/// cap keeps a hostile peer from filling the entire response budget with
/// header bytes before any body has been seen.
pub const MAX_HEADER_BYTES: usize = 32 * 1024;

/// Independent bound on any single chunk in a `Transfer-Encoding: chunked`
/// response. Also caps the size we're willing to read past a chunk header
/// before finding its `\r\n` terminator.
pub const MAX_CHUNK_BYTES: usize = MAX_RESPONSE_BYTES;

/// Fixed plug reference for the myna client. Snapd only ever sees this plug
/// on the myna:backend side of the connection.
pub const MYNA_PLUG_SNAP: &str = "myna";
pub const MYNA_PLUG_NAME: &str = "backend";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapdTimeoutContext {
    Request,
    ChangePolling,
}

impl SnapdTimeoutContext {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Request => "snapd request",
            Self::ChangePolling => "snapd change polling",
        }
    }
}

/// Errors that can be returned by the snapd REST client. These map directly to
/// the higher-level [`crate::ports::SystemConfiguratorError`] variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapdError {
    /// Operation was cancelled before or during the request, or the user
    /// dismissed snapd's authorization prompt (`kind: "auth-cancelled"`).
    Cancelled,
    /// Per-request or total timeout expired.
    Timeout {
        /// Duration reported to the caller in the error message.
        elapsed: Duration,
        /// What timed out.
        context: SnapdTimeoutContext,
    },
    /// Transport failure: could not connect, read, or write to the socket.
    Transport { message: String },
    /// The response could not be parsed as a valid snapd envelope.
    Protocol { message: String, body: String },
    /// The response exceeded [`MAX_RESPONSE_BYTES`].
    ResponseTooLarge,
    /// Snapd returned an error envelope (`type: "error"`) or the change
    /// finished with an error.
    Snapd {
        /// HTTP status code returned by snapd.
        status_code: u16,
        /// Optional `kind` field from the snapd error result.
        kind: Option<String>,
        /// The `message` field from the snapd error result.
        message: String,
    },
    /// Polkit or an equivalent authorization mechanism denied the request
    /// (HTTP 401 / 403 or snapd error `kind: "login-required"`).
    AuthorizationDenied {
        status_code: u16,
        kind: Option<String>,
        message: String,
    },
}

/// A blocking snapd worker that panicked, with the panic's message when it
/// has one.
fn worker_panicked(payload: Box<dyn std::any::Any + Send>) -> SnapdError {
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    SnapdError::Transport {
        message: match message {
            Some(message) => format!("snapd worker panicked: {message}"),
            None => "snapd worker panicked".to_owned(),
        },
    }
}

impl std::fmt::Display for SnapdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("snapd request cancelled"),
            Self::Timeout { elapsed, context } => {
                write!(
                    f,
                    "{} timed out after {}",
                    context.description(),
                    crate::command::duration_text(*elapsed)
                )
            }
            Self::Transport { message } => write!(f, "snapd transport error: {message}"),
            Self::Protocol { message, .. } => write!(f, "snapd protocol error: {message}"),
            Self::ResponseTooLarge => f.write_str("snapd response exceeded size limit"),
            Self::Snapd {
                status_code,
                message,
                ..
            } => write!(f, "snapd rejected request (HTTP {status_code}): {message}"),
            Self::AuthorizationDenied {
                status_code,
                message,
                ..
            } => write!(
                f,
                "snapd authorization denied (HTTP {status_code}): {message}"
            ),
        }
    }
}

impl std::error::Error for SnapdError {}

/// One typed interface action the client is allowed to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceAction {
    Connect {
        backend_snap: String,
        backend_slot: String,
    },
    Disconnect {
        backend_snap: String,
        backend_slot: String,
    },
}

impl InterfaceAction {
    pub fn backend_snap(&self) -> &str {
        match self {
            Self::Connect { backend_snap, .. } | Self::Disconnect { backend_snap, .. } => {
                backend_snap
            }
        }
    }

    pub fn backend_slot(&self) -> &str {
        match self {
            Self::Connect { backend_slot, .. } | Self::Disconnect { backend_slot, .. } => {
                backend_slot
            }
        }
    }

    pub fn action_str(&self) -> &'static str {
        match self {
            Self::Connect { .. } => "connect",
            Self::Disconnect { .. } => "disconnect",
        }
    }

    /// Validate the action and produce the exact JSON body snapd expects.
    /// Rejects any snap or slot name that does not match snapd's grammar.
    pub fn to_request_body(&self) -> Result<String, SnapdError> {
        let backend = self.backend_snap();
        if !is_valid_snap_name(backend) {
            return Err(SnapdError::Transport {
                message: format!("invalid backend snap name: {backend}"),
            });
        }
        let slot = self.backend_slot();
        if !is_valid_slot_name(slot) {
            return Err(SnapdError::Transport {
                message: format!("invalid backend slot name: {slot}"),
            });
        }
        let request = InterfaceRequest {
            action: self.action_str(),
            plugs: [Plug {
                snap: MYNA_PLUG_SNAP,
                plug: MYNA_PLUG_NAME,
            }],
            slots: [Slot {
                snap: backend,
                slot,
            }],
        };
        serde_json::to_string(&request).map_err(|error| SnapdError::Transport {
            message: format!("could not encode snapd request: {error}"),
        })
    }
}

#[derive(Serialize)]
struct InterfaceRequest<'a> {
    action: &'a str,
    plugs: [Plug<'a>; 1],
    slots: [Slot<'a>; 1],
}

#[derive(Serialize)]
struct Plug<'a> {
    snap: &'a str,
    plug: &'a str,
}

#[derive(Serialize)]
struct Slot<'a> {
    snap: &'a str,
    slot: &'a str,
}

/// Reject any snap name that does not match the standard snap name grammar
/// (`[a-z0-9][a-z0-9-]*[a-z0-9]`, no double hyphens, length 1..=40).
/// snapd's plug/slot name grammar: `^[a-z](?:-?[a-z0-9])*$`.
pub fn is_valid_slot_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_lowercase) || bytes.last() == Some(&b'-') {
        return false;
    }
    !name.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

pub fn is_valid_snap_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 40 {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if *bytes.last().unwrap() == b'-' {
        return false;
    }
    let mut prev_hyphen = false;
    for &b in bytes {
        match b {
            b'a'..=b'z' | b'0'..=b'9' => prev_hyphen = false,
            b'-' => {
                if prev_hyphen {
                    return false;
                }
                prev_hyphen = true;
            }
            _ => return false,
        }
    }
    true
}

/// Reject any change identifier that would introduce path segments,
/// alter the request line, or smuggle a header into the URL. snapd's
/// change IDs are opaque tokens; in practice they are short decimal
/// strings, but we deliberately allow the wider ASCII-alphanumeric plus
/// `-_` set to accommodate future formats while still refusing anything
/// that would alter the endpoint. See design note in
/// `config-ui/confinement.md`.
pub fn is_valid_change_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    id.bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'))
}

/// A completed successful change reported by snapd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeReport {
    pub change_id: String,
    pub status: String,
}

/// Result of an [`InterfaceAction`] executed against snapd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapdOutcome {
    Sync,
    Async(ChangeReport),
}

/// Timeouts used by [`UnixSocketSnapdClient`].
#[derive(Clone, Copy, Debug)]
pub struct SnapdTimeouts {
    pub per_request: Duration,
    /// How long a write may wait for its answer: snapd answers only once the
    /// user has answered its polkit prompt.
    pub authorization: Duration,
    pub poll_interval: Duration,
    pub total: Duration,
}

impl Default for SnapdTimeouts {
    fn default() -> Self {
        Self {
            per_request: Duration::from_secs(30),
            authorization: Duration::from_secs(10 * 60),
            poll_interval: Duration::from_millis(500),
            total: Duration::from_secs(120),
        }
    }
}

/// Async trait for executing a single validated interface action against snapd.
#[async_trait(?Send)]
pub trait SnapdClient {
    async fn apply_interface_action(
        &self,
        action: InterfaceAction,
        cancellation: CancellationToken,
    ) -> Result<SnapdOutcome, SnapdError>;

    /// The changes snapd has not finished, read as the user.
    async fn changes_in_progress(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ChangeInProgress>, SnapdError> {
        Ok(Vec::new())
    }

    /// Whether `experimental.user-daemons` is on, read as the user.
    async fn user_daemons_enabled(
        &self,
        cancellation: CancellationToken,
    ) -> Result<bool, SnapdError>;

    /// Start installing `snap` from edge; snapd asks polkit
    /// for `manage`. The change it started, none when the snap is already
    /// installed.
    async fn install_snap(
        &self,
        snap: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<String>, SnapdError>;

    /// One change as snapd reports it now, read as the user.
    async fn change(
        &self,
        change_id: &str,
        cancellation: CancellationToken,
    ) -> Result<ChangeInProgress, SnapdError>;
}

/// Real snapd client. Runs blocking Unix-socket I/O off the GTK main loop via
/// [`gio::spawn_blocking`] so callers on the main context are never blocked.
#[derive(Clone, Debug)]
pub struct UnixSocketSnapdClient {
    socket_path: PathBuf,
    timeouts: SnapdTimeouts,
}

impl UnixSocketSnapdClient {
    pub fn new() -> Self {
        Self::with_socket(DEFAULT_SNAPD_SOCKET)
    }

    pub fn with_socket(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
            timeouts: SnapdTimeouts::default(),
        }
    }

    pub fn with_timeouts(mut self, timeouts: SnapdTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
}

impl Default for UnixSocketSnapdClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl SnapdClient for UnixSocketSnapdClient {
    async fn apply_interface_action(
        &self,
        action: InterfaceAction,
        cancellation: CancellationToken,
    ) -> Result<SnapdOutcome, SnapdError> {
        let socket_path = self.socket_path.clone();
        let timeouts = self.timeouts;
        // Move all blocking socket I/O off the GTK main loop.
        let handle = gio::spawn_blocking(move || {
            blocking_apply_interface_action(&socket_path, timeouts, action, cancellation)
        });
        handle.await.map_err(worker_panicked)?
    }

    async fn changes_in_progress(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ChangeInProgress>, SnapdError> {
        let socket_path = self.socket_path.clone();
        let timeouts = self.timeouts;
        let handle = gio::spawn_blocking(move || {
            blocking_changes_in_progress(&socket_path, timeouts, cancellation)
        });
        handle.await.map_err(worker_panicked)?
    }

    async fn user_daemons_enabled(
        &self,
        cancellation: CancellationToken,
    ) -> Result<bool, SnapdError> {
        let socket_path = self.socket_path.clone();
        let timeouts = self.timeouts;
        let handle = gio::spawn_blocking(move || {
            blocking_user_daemons_enabled(&socket_path, timeouts, cancellation)
        });
        handle.await.map_err(worker_panicked)?
    }

    async fn install_snap(
        &self,
        snap: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<String>, SnapdError> {
        if !is_valid_snap_name(snap) {
            return Err(SnapdError::Transport {
                message: format!("invalid snap name: {snap}"),
            });
        }
        let socket_path = self.socket_path.clone();
        let timeouts = self.timeouts;
        let path = format!("/v2/snaps/{snap}");
        let handle = gio::spawn_blocking(move || {
            blocking_start(
                &socket_path,
                timeouts,
                "POST",
                &path,
                INSTALL_ON_EDGE,
                &cancellation,
            )
        });
        let started = handle.await.map_err(worker_panicked)?;
        match started {
            Ok(Started::Change(change_id)) => Ok(Some(change_id)),
            Ok(Started::Sync) => Err(SnapdError::Protocol {
                message: "snapd answered an install synchronously".to_owned(),
                body: String::new(),
            }),
            Err(SnapdError::Snapd {
                kind: Some(kind), ..
            }) if kind == "snap-already-installed" => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn change(
        &self,
        change_id: &str,
        cancellation: CancellationToken,
    ) -> Result<ChangeInProgress, SnapdError> {
        if !is_valid_change_id(change_id) {
            return Err(SnapdError::Protocol {
                message: format!("refusing to read invalid change id: {change_id:?}"),
                body: String::new(),
            });
        }
        let socket_path = self.socket_path.clone();
        let timeouts = self.timeouts;
        let path = format!("/v2/changes/{change_id}");
        let handle = gio::spawn_blocking(move || {
            blocking_change(&socket_path, timeouts, &path, cancellation)
        });
        handle.await.map_err(worker_panicked)?
    }
}

fn blocking_change(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    path: &str,
    cancellation: CancellationToken,
) -> Result<ChangeInProgress, SnapdError> {
    let start = Instant::now();
    let response = do_request(
        socket_path,
        "GET",
        path,
        None,
        SnapdTimeoutContext::Request,
        &timeouts,
        &cancellation,
        start,
        start + timeouts.per_request,
    )?;
    match parse_envelope(&response)? {
        Envelope::Sync { result_json } => {
            parse_change(result_json).map_err(|message| SnapdError::Protocol {
                message: format!("could not parse the change: {message}"),
                body: truncate(&response, 512),
            })
        }
        Envelope::Async { .. } => Err(SnapdError::Protocol {
            message: "unexpected async envelope for a change".to_owned(),
            body: truncate(&response, 512),
        }),
        Envelope::Error {
            status_code,
            kind,
            message,
        } => Err(classify_error(status_code, kind, message)),
    }
}

fn blocking_user_daemons_enabled(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    cancellation: CancellationToken,
) -> Result<bool, SnapdError> {
    let start = Instant::now();
    let response = do_request(
        socket_path,
        "GET",
        SYSTEM_INFO,
        None,
        SnapdTimeoutContext::Request,
        &timeouts,
        &cancellation,
        start,
        start + timeouts.per_request,
    )?;
    match parse_envelope(&response)? {
        Envelope::Sync { result_json } => Ok(user_daemons_feature(&result_json)),
        Envelope::Async { .. } => Err(SnapdError::Protocol {
            message: "unexpected async envelope for the system information".to_owned(),
            body: truncate(&response, 512),
        }),
        Envelope::Error {
            status_code,
            kind,
            message,
        } => Err(classify_error(status_code, kind, message)),
    }
}

/// A snapd that does not list the flag cannot have it on.
fn user_daemons_feature(system_info: &serde_json::Value) -> bool {
    system_info["features"]["user-daemons"]["enabled"]
        .as_bool()
        .unwrap_or(false)
}

fn blocking_changes_in_progress(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    cancellation: CancellationToken,
) -> Result<Vec<ChangeInProgress>, SnapdError> {
    let start = Instant::now();
    let response = do_request(
        socket_path,
        "GET",
        CHANGES_IN_PROGRESS,
        None,
        SnapdTimeoutContext::Request,
        &timeouts,
        &cancellation,
        start,
        start + timeouts.per_request,
    )?;
    match parse_envelope(&response)? {
        Envelope::Sync { result_json } => {
            parse_changes(result_json).map_err(|message| SnapdError::Protocol {
                message: format!("could not parse the changes list: {message}"),
                body: truncate(&response, 512),
            })
        }
        Envelope::Async { .. } => Err(SnapdError::Protocol {
            message: "unexpected async envelope for the changes list".to_owned(),
            body: truncate(&response, 512),
        }),
        Envelope::Error {
            status_code,
            kind,
            message,
        } => Err(classify_error(status_code, kind, message)),
    }
}

fn blocking_apply_interface_action(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    action: InterfaceAction,
    cancellation: CancellationToken,
) -> Result<SnapdOutcome, SnapdError> {
    if cancellation.is_cancelled() {
        return Err(SnapdError::Cancelled);
    }
    let body = action.to_request_body()?;
    blocking_write(
        socket_path,
        timeouts,
        "POST",
        INTERFACES,
        &body,
        cancellation,
    )
}

/// What a write started.
enum Started {
    Sync,
    Change(String),
}

/// Send one privileged write. snapd answers only once the user has answered
/// its polkit prompt, so the request waits `timeouts.authorization`.
fn blocking_start(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    method: &str,
    path: &str,
    body: &str,
    cancellation: &CancellationToken,
) -> Result<Started, SnapdError> {
    let start = Instant::now();
    let answering = SnapdTimeouts {
        per_request: timeouts.authorization,
        ..timeouts
    };
    let response = do_request(
        socket_path,
        method,
        path,
        Some(body),
        SnapdTimeoutContext::Request,
        &answering,
        cancellation,
        start,
        start + timeouts.authorization,
    )?;
    match parse_envelope(&response)? {
        Envelope::Sync { .. } => Ok(Started::Sync),
        Envelope::Async { change_id } if is_valid_change_id(&change_id) => {
            Ok(Started::Change(change_id))
        }
        Envelope::Async { change_id } => Err(SnapdError::Protocol {
            message: format!("snapd returned an invalid change id: {change_id:?}"),
            body: truncate(&response, 512),
        }),
        Envelope::Error {
            status_code,
            kind,
            message,
        } => Err(classify_error(status_code, kind, message)),
    }
}

/// Send one privileged write and follow its change. The request may wait
/// out the user's polkit prompt; the change then gets the total budget.
fn blocking_write(
    socket_path: &Path,
    timeouts: SnapdTimeouts,
    method: &str,
    path: &str,
    body: &str,
    cancellation: CancellationToken,
) -> Result<SnapdOutcome, SnapdError> {
    let start = Instant::now();
    match blocking_start(socket_path, timeouts, method, path, body, &cancellation)? {
        Started::Sync => Ok(SnapdOutcome::Sync),
        Started::Change(change_id) => {
            let answered = Instant::now();
            poll_change(
                socket_path,
                &change_id,
                &timeouts,
                &cancellation,
                start,
                answered + timeouts.total,
            )
            .map(SnapdOutcome::Async)
        }
    }
}

fn poll_change(
    socket_path: &Path,
    change_id: &str,
    timeouts: &SnapdTimeouts,
    cancellation: &CancellationToken,
    start: Instant,
    deadline: Instant,
) -> Result<ChangeReport, SnapdError> {
    // Defence in depth: the caller has already validated the change id, but
    // never interpolate an unvalidated one into a request path or header.
    debug_assert!(is_valid_change_id(change_id));
    if !is_valid_change_id(change_id) {
        return Err(SnapdError::Protocol {
            message: format!("refusing to poll invalid change id: {change_id:?}"),
            body: String::new(),
        });
    }
    let path = format!("/v2/changes/{change_id}");
    loop {
        if cancellation.is_cancelled() {
            return Err(SnapdError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(SnapdError::Timeout {
                elapsed: start.elapsed(),
                context: SnapdTimeoutContext::ChangePolling,
            });
        }
        let response = do_request(
            socket_path,
            "GET",
            &path,
            None,
            SnapdTimeoutContext::ChangePolling,
            timeouts,
            cancellation,
            start,
            deadline,
        )?;
        match parse_envelope(&response)? {
            Envelope::Sync { result_json } => {
                let change: ChangeBody =
                    serde_json::from_value(result_json.clone()).map_err(|error| {
                        SnapdError::Protocol {
                            message: format!("could not parse change body: {error}"),
                            body: result_json.to_string(),
                        }
                    })?;
                if change.ready {
                    if let Some(err) = change.err {
                        return Err(SnapdError::Snapd {
                            status_code: 200,
                            kind: Some("change-failed".to_owned()),
                            message: err,
                        });
                    }
                    return Ok(ChangeReport {
                        change_id: change_id.to_owned(),
                        status: change.status,
                    });
                }
            }
            Envelope::Async { .. } => {
                return Err(SnapdError::Protocol {
                    message: "unexpected async envelope while polling change".to_owned(),
                    body: response,
                })
            }
            Envelope::Error {
                status_code,
                kind,
                message,
            } => return Err(classify_error(status_code, kind, message)),
        }
        // Sleep a bounded interval, but respect cancellation and deadline.
        let poll_end = Instant::now() + timeouts.poll_interval;
        while Instant::now() < poll_end {
            if cancellation.is_cancelled() {
                return Err(SnapdError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(25).min(timeouts.poll_interval));
        }
    }
}

#[derive(Deserialize)]
struct ChangeBody {
    ready: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    err: Option<String>,
}

/// Snapd HTTP+JSON envelope.
#[derive(Debug)]
enum Envelope {
    Sync {
        result_json: serde_json::Value,
    },
    Async {
        change_id: String,
    },
    Error {
        status_code: u16,
        kind: Option<String>,
        message: String,
    },
}

fn parse_envelope(body: &str) -> Result<Envelope, SnapdError> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default, rename = "status-code")]
        status_code: Option<u16>,
        #[serde(default)]
        change: Option<String>,
        #[serde(default)]
        result: serde_json::Value,
    }
    let raw: Raw = serde_json::from_str(body).map_err(|error| SnapdError::Protocol {
        message: format!("invalid snapd envelope: {error}"),
        body: truncate(body, 512),
    })?;
    let status_code = raw.status_code.unwrap_or(0);
    match raw.kind.as_str() {
        "sync" => Ok(Envelope::Sync {
            result_json: raw.result,
        }),
        "async" => {
            let change_id = raw.change.ok_or_else(|| SnapdError::Protocol {
                message: "async envelope missing change id".to_owned(),
                body: truncate(body, 512),
            })?;
            Ok(Envelope::Async { change_id })
        }
        "error" => {
            #[derive(Deserialize)]
            struct ErrResult {
                #[serde(default)]
                message: String,
                #[serde(default)]
                kind: Option<String>,
            }
            let details: ErrResult = serde_json::from_value(raw.result).unwrap_or(ErrResult {
                message: "snapd returned an error without a result body".to_owned(),
                kind: None,
            });
            Ok(Envelope::Error {
                status_code,
                kind: details.kind,
                message: if details.message.is_empty() {
                    "snapd returned an error without a message".to_owned()
                } else {
                    details.message
                },
            })
        }
        other => Err(SnapdError::Protocol {
            message: format!("unknown snapd envelope type: {other}"),
            body: truncate(body, 512),
        }),
    }
}

fn classify_error(status_code: u16, kind: Option<String>, message: String) -> SnapdError {
    if kind.as_deref() == Some("auth-cancelled") {
        return SnapdError::Cancelled;
    }
    let is_auth_kind = matches!(
        kind.as_deref(),
        Some("login-required") | Some("interactive-required")
    );
    if status_code == 401 || status_code == 403 || is_auth_kind {
        SnapdError::AuthorizationDenied {
            status_code,
            kind,
            message,
        }
    } else {
        SnapdError::Snapd {
            status_code,
            kind,
            message,
        }
    }
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        input.to_owned()
    } else {
        let mut cut = max;
        while !input.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &input[..cut])
    }
}

#[allow(clippy::too_many_arguments)]
fn do_request(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout_context: SnapdTimeoutContext,
    timeouts: &SnapdTimeouts,
    cancellation: &CancellationToken,
    start: Instant,
    deadline: Instant,
) -> Result<String, SnapdError> {
    if cancellation.is_cancelled() {
        return Err(SnapdError::Cancelled);
    }
    let now = Instant::now();
    if now >= deadline {
        return Err(SnapdError::Timeout {
            elapsed: start.elapsed(),
            context: timeout_context,
        });
    }
    let call_deadline = (now + timeouts.per_request).min(deadline);

    let mut stream = UnixStream::connect(socket_path).map_err(|error| SnapdError::Transport {
        message: format!("connect {}: {}", socket_path.display(), error),
    })?;
    stream
        .set_read_timeout(Some(remaining(call_deadline, start, timeout_context)?))
        .map_err(|error| transport_err(error, start, timeout_context))?;
    stream
        .set_write_timeout(Some(remaining(call_deadline, start, timeout_context)?))
        .map_err(|error| transport_err(error, start, timeout_context))?;

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: snapd\r\nAccept: application/json\r\nX-Allow-Interaction: true\r\nConnection: close\r\n"
    );
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        ));
        request.push_str(body);
    } else {
        request.push_str("\r\n");
    }
    stream
        .write_all(request.as_bytes())
        .map_err(|error| transport_err(error, start, timeout_context))?;
    stream
        .flush()
        .map_err(|error| transport_err(error, start, timeout_context))?;

    read_response(
        &mut stream,
        cancellation,
        start,
        call_deadline,
        timeout_context,
    )
}

/// One `read` bounded by `deadline`. A signal (SIGCHLD from a child this
/// process spawned, a SIGCONT) interrupts a socket read that has a timeout
/// even under `SA_RESTART`; that is not snapd's doing, so it is retried.
fn read_some(
    stream: &mut UnixStream,
    scratch: &mut [u8],
    start: Instant,
    deadline: Instant,
    context: SnapdTimeoutContext,
) -> Result<usize, SnapdError> {
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline, start, context)?))
            .map_err(|error| transport_err(error, start, context))?;
        match stream.read(scratch) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            other => return other.map_err(|error| transport_err(error, start, context)),
        }
    }
}

fn transport_err(error: io::Error, start: Instant, context: SnapdTimeoutContext) -> SnapdError {
    if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::TimedOut {
        SnapdError::Timeout {
            elapsed: start.elapsed(),
            context,
        }
    } else {
        SnapdError::Transport {
            message: error.to_string(),
        }
    }
}

fn remaining(
    deadline: Instant,
    start: Instant,
    context: SnapdTimeoutContext,
) -> Result<Duration, SnapdError> {
    let now = Instant::now();
    if now >= deadline {
        Err(SnapdError::Timeout {
            elapsed: start.elapsed(),
            context,
        })
    } else {
        Ok(deadline - now)
    }
}

fn read_response(
    stream: &mut UnixStream,
    cancellation: &CancellationToken,
    start: Instant,
    deadline: Instant,
    timeout_context: SnapdTimeoutContext,
) -> Result<String, SnapdError> {
    let mut buffer = Vec::with_capacity(8 * 1024);
    let mut scratch = [0u8; 8 * 1024];
    // Read the header block first.
    let header_end;
    loop {
        if cancellation.is_cancelled() {
            return Err(SnapdError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(SnapdError::Timeout {
                elapsed: start.elapsed(),
                context: timeout_context,
            });
        }
        if let Some(index) = find_double_crlf(&buffer) {
            header_end = index;
            break;
        }
        // Header block has its own bound independent of the body cap; a
        // hostile peer cannot exhaust the response budget with header bytes.
        if buffer.len() >= MAX_HEADER_BYTES {
            return Err(SnapdError::Protocol {
                message: format!(
                    "snapd response headers exceeded {MAX_HEADER_BYTES} bytes without a CRLF CRLF terminator"
                ),
                body: String::new(),
            });
        }
        let bytes = read_some(stream, &mut scratch, start, deadline, timeout_context)?;
        if bytes == 0 {
            return Err(SnapdError::Protocol {
                message: "snapd closed the connection before completing headers".to_owned(),
                body: String::from_utf8_lossy(&buffer).into_owned(),
            });
        }
        buffer.extend_from_slice(&scratch[..bytes]);
    }
    let header_text =
        std::str::from_utf8(&buffer[..header_end]).map_err(|error| SnapdError::Protocol {
            message: format!("invalid header bytes: {error}"),
            body: String::new(),
        })?;
    let headers = parse_headers(header_text)?;

    let body_bytes = if let Some(len) = headers.content_length {
        if len > MAX_RESPONSE_BYTES {
            return Err(SnapdError::ResponseTooLarge);
        }
        let mut body: Vec<u8> = buffer[header_end + 4..].to_vec();
        while body.len() < len {
            if cancellation.is_cancelled() {
                return Err(SnapdError::Cancelled);
            }
            let bytes = read_some(stream, &mut scratch, start, deadline, timeout_context)?;
            if bytes == 0 {
                return Err(SnapdError::Protocol {
                    message: "snapd closed the connection before Content-Length was satisfied"
                        .to_owned(),
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
            }
            if body.len() + bytes > MAX_RESPONSE_BYTES {
                return Err(SnapdError::ResponseTooLarge);
            }
            body.extend_from_slice(&scratch[..bytes]);
        }
        body.truncate(len);
        body
    } else if headers.chunked {
        let mut body = Vec::new();
        let mut pending: Vec<u8> = buffer[header_end + 4..].to_vec();
        loop {
            match decode_chunked(&mut pending)? {
                ChunkStep::Complete => break,
                ChunkStep::NeedMore => {
                    if cancellation.is_cancelled() {
                        return Err(SnapdError::Cancelled);
                    }
                    if pending.len() >= MAX_CHUNK_BYTES {
                        return Err(SnapdError::ResponseTooLarge);
                    }
                    let bytes = read_some(stream, &mut scratch, start, deadline, timeout_context)?;
                    if bytes == 0 {
                        return Err(SnapdError::Protocol {
                            message: "snapd closed the connection during chunked transfer"
                                .to_owned(),
                            body: String::from_utf8_lossy(&pending).into_owned(),
                        });
                    }
                    pending.extend_from_slice(&scratch[..bytes]);
                }
                ChunkStep::Chunk(chunk) => {
                    if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Err(SnapdError::ResponseTooLarge);
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
        body
    } else {
        // Read until close. Explicit `Connection: close` guarantees this is
        // well defined for snapd's HTTP/1.1 responses.
        let mut body: Vec<u8> = buffer[header_end + 4..].to_vec();
        loop {
            if cancellation.is_cancelled() {
                return Err(SnapdError::Cancelled);
            }
            let bytes = read_some(stream, &mut scratch, start, deadline, timeout_context)?;
            if bytes == 0 {
                break;
            }
            if body.len() + bytes > MAX_RESPONSE_BYTES {
                return Err(SnapdError::ResponseTooLarge);
            }
            body.extend_from_slice(&scratch[..bytes]);
        }
        body
    };

    let body_string = String::from_utf8(body_bytes).map_err(|error| SnapdError::Protocol {
        message: format!("body was not valid UTF-8: {error}"),
        body: String::new(),
    })?;

    // Reject any HTTP status outside 2xx that snapd did not accompany with an
    // error-typed envelope. Callers rely on the envelope-type dispatch and
    // this makes HTTP-status and envelope-status handling consistent: a
    // non-2xx response is only accepted when parse_envelope classifies it as
    // Envelope::Error and classify_error maps it.
    if !(200..300).contains(&headers.status_code) {
        // Try to parse; if it's an error envelope, let the caller dispatch it.
        // Otherwise surface a Snapd/HTTP error carrying the truncated body.
        return match parse_envelope(&body_string) {
            Ok(Envelope::Error { .. }) => Ok(body_string),
            _ => Err(SnapdError::Snapd {
                status_code: headers.status_code,
                kind: None,
                message: format!(
                    "snapd HTTP {}, body: {}",
                    headers.status_code,
                    truncate(&body_string, 512)
                ),
            }),
        };
    }
    Ok(body_string)
}

enum ChunkStep {
    Chunk(Vec<u8>),
    NeedMore,
    Complete,
}

fn decode_chunked(buffer: &mut Vec<u8>) -> Result<ChunkStep, SnapdError> {
    let Some(line_end) = find_crlf(buffer) else {
        if buffer.len() > MAX_CHUNK_BYTES {
            return Err(SnapdError::ResponseTooLarge);
        }
        return Ok(ChunkStep::NeedMore);
    };
    let header =
        std::str::from_utf8(&buffer[..line_end]).map_err(|error| SnapdError::Protocol {
            message: format!("invalid chunk header: {error}"),
            body: String::new(),
        })?;
    let size_str = header.split(';').next().unwrap_or("").trim();
    // usize::from_str_radix accepts a leading '+' — reject non-hex prefixes so
    // a hostile peer cannot use the chunk length parser to smuggle values.
    if size_str.is_empty() || !size_str.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SnapdError::Protocol {
            message: format!("invalid chunk length {size_str:?}"),
            body: String::new(),
        });
    }
    let size = usize::from_str_radix(size_str, 16).map_err(|error| SnapdError::Protocol {
        message: format!("invalid chunk length {size_str:?}: {error}"),
        body: String::new(),
    })?;
    if size > MAX_CHUNK_BYTES {
        return Err(SnapdError::ResponseTooLarge);
    }
    let needed = line_end + 2 + size + 2;
    if buffer.len() < needed {
        return Ok(ChunkStep::NeedMore);
    }
    // Every chunk (including the terminating zero chunk) MUST be followed by
    // a CRLF. Reject anything else so no smuggled bytes can bleed into the
    // parsed body.
    let trailer_start = line_end + 2 + size;
    if &buffer[trailer_start..trailer_start + 2] != b"\r\n" {
        return Err(SnapdError::Protocol {
            message: "chunk not terminated by CRLF".to_owned(),
            body: String::new(),
        });
    }
    if size == 0 {
        // Zero chunk: consume the size line and its terminating CRLF only.
        // Any real trailers between the two CRLFs would need dedicated
        // parsing; snapd never sends them today, so we reject to avoid
        // silently ignoring smuggled headers.
        buffer.drain(..needed);
        Ok(ChunkStep::Complete)
    } else {
        let chunk = buffer[line_end + 2..line_end + 2 + size].to_vec();
        buffer.drain(..needed);
        Ok(ChunkStep::Chunk(chunk))
    }
}

fn find_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|w| w == b"\r\n")
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Debug)]
struct Headers {
    status_code: u16,
    content_length: Option<usize>,
    chunked: bool,
}

fn parse_headers(text: &str) -> Result<Headers, SnapdError> {
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| SnapdError::Protocol {
        message: "missing HTTP status line".to_owned(),
        body: String::new(),
    })?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(SnapdError::Protocol {
            message: format!("unsupported HTTP version: {version}"),
            body: status_line.to_owned(),
        });
    }
    let code_str = parts.next().unwrap_or("");
    let status_code: u16 = code_str.parse().map_err(|_| SnapdError::Protocol {
        message: format!("invalid HTTP status code: {code_str:?}"),
        body: status_line.to_owned(),
    })?;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut saw_transfer_encoding = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // RFC 9112 §6.3 — reject ambiguous/duplicate Content-Length. This
            // includes both a repeated header and a single header carrying a
            // comma-separated list whose values disagree.
            let parts_iter = value.split(',').map(str::trim).filter(|s| !s.is_empty());
            let mut agreed: Option<usize> = None;
            for part in parts_iter {
                let parsed: usize = part.parse().map_err(|_| SnapdError::Protocol {
                    message: format!("invalid Content-Length: {value:?}"),
                    body: line.to_owned(),
                })?;
                match agreed {
                    None => agreed = Some(parsed),
                    Some(existing) if existing == parsed => {}
                    Some(_) => {
                        return Err(SnapdError::Protocol {
                            message: format!("conflicting Content-Length values: {value:?}"),
                            body: line.to_owned(),
                        });
                    }
                }
            }
            let Some(new_len) = agreed else {
                return Err(SnapdError::Protocol {
                    message: format!("invalid Content-Length: {value:?}"),
                    body: line.to_owned(),
                });
            };
            if let Some(existing) = content_length {
                if existing != new_len {
                    return Err(SnapdError::Protocol {
                        message: format!(
                            "conflicting Content-Length headers: {existing} vs {new_len}"
                        ),
                        body: line.to_owned(),
                    });
                }
            }
            content_length = Some(new_len);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            saw_transfer_encoding = true;
            // Only bare `chunked` is supported. Anything else — including
            // `identity`, `gzip`, `deflate`, or a comma-separated codings
            // list — is rejected so snapd's actual framing is unambiguous.
            let mut codings = value.split(',').map(str::trim).filter(|s| !s.is_empty());
            let Some(single) = codings.next() else {
                return Err(SnapdError::Protocol {
                    message: "empty Transfer-Encoding".to_owned(),
                    body: line.to_owned(),
                });
            };
            if codings.next().is_some() {
                return Err(SnapdError::Protocol {
                    message: format!("unsupported Transfer-Encoding: {value:?}"),
                    body: line.to_owned(),
                });
            }
            if !single.eq_ignore_ascii_case("chunked") {
                return Err(SnapdError::Protocol {
                    message: format!("unsupported Transfer-Encoding: {value:?}"),
                    body: line.to_owned(),
                });
            }
            chunked = true;
        }
    }
    // RFC 9112 §6.1 — if both are present, Transfer-Encoding wins and
    // Content-Length must be ignored, but that combination is ambiguous
    // enough that many gateways treat it as a request-smuggling signal. We
    // reject it outright.
    if saw_transfer_encoding && content_length.is_some() {
        return Err(SnapdError::Protocol {
            message: "response declared both Transfer-Encoding and Content-Length".to_owned(),
            body: String::new(),
        });
    }
    Ok(Headers {
        status_code,
        content_length,
        chunked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn ignore_signal(_: libc::c_int) {}

    /// A child exiting delivers SIGCHLD, which interrupts a socket read that
    /// has a timeout even under `SA_RESTART`. That must not fail the request.
    #[test]
    fn a_signal_during_a_read_does_not_fail_it() {
        // SAFETY: installs a no-op handler, so SIGUSR1 to this test binary
        // no longer terminates it; nothing else in the binary uses SIGUSR1.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = ignore_signal as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
                0
            );
        }
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        // SAFETY: pthread_self has no preconditions.
        let me = unsafe { libc::pthread_self() } as usize;
        let poker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            // SAFETY: `me` is the test thread, alive until it joins us.
            unsafe { libc::pthread_kill(me as libc::pthread_t, libc::SIGUSR1) };
            std::thread::sleep(Duration::from_millis(100));
            writer.write_all(b"ok").unwrap();
        });
        let start = Instant::now();
        let mut scratch = [0u8; 8];
        let read = read_some(
            &mut reader,
            &mut scratch,
            start,
            start + Duration::from_secs(10),
            SnapdTimeoutContext::Request,
        );
        poker.join().unwrap();
        assert_eq!(read, Ok(2));
    }

    #[test]
    fn snap_name_validation_matches_grammar() {
        assert!(is_valid_snap_name("myna-parakeet"));
        assert!(is_valid_snap_name("whisper"));
        assert!(is_valid_snap_name("a"));
        assert!(!is_valid_snap_name(""));
        assert!(!is_valid_snap_name("-leading"));
        assert!(!is_valid_snap_name("trailing-"));
        assert!(!is_valid_snap_name("double--hyphen"));
        assert!(!is_valid_snap_name("Upper"));
        assert!(!is_valid_snap_name("has space"));
        assert!(!is_valid_snap_name(&"x".repeat(41)));
    }

    #[test]
    fn interface_action_body_uses_myna_backend_plug_and_typed_slot() {
        let body = InterfaceAction::Connect {
            backend_snap: "myna-parakeet".to_owned(),
            backend_slot: "provider".to_owned(),
        }
        .to_request_body()
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["action"], "connect");
        assert_eq!(value["plugs"][0]["snap"], "myna");
        assert_eq!(value["plugs"][0]["plug"], "backend");
        assert!(value["plugs"][0].get("name").is_none());
        assert_eq!(value["slots"][0]["snap"], "myna-parakeet");
        assert_eq!(value["slots"][0]["slot"], "provider");
        assert!(value["slots"][0].get("name").is_none());
    }

    #[test]
    fn interface_action_body_is_exact_for_a_provider_slot() {
        let body = InterfaceAction::Connect {
            backend_snap: "myna-parakeet".to_owned(),
            backend_slot: "provider".to_owned(),
        }
        .to_request_body()
        .unwrap();
        assert_eq!(
            body,
            r#"{"action":"connect","plugs":[{"snap":"myna","plug":"backend"}],"slots":[{"snap":"myna-parakeet","slot":"provider"}]}"#
        );
    }

    #[test]
    fn interface_action_body_uses_the_recorded_slot_name() {
        let body = InterfaceAction::Disconnect {
            backend_snap: "community-asr".to_owned(),
            backend_slot: "speech".to_owned(),
        }
        .to_request_body()
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["action"], "disconnect");
        assert_eq!(value["slots"][0]["slot"], "speech");
    }

    #[test]
    fn interface_action_rejects_invalid_slot_name() {
        for slot in [
            "",
            "Provider",
            "9provider",
            "-provider",
            "provider-",
            "pro--vider",
            "a:b",
        ] {
            assert!(
                matches!(
                    InterfaceAction::Connect {
                        backend_snap: "myna-parakeet".to_owned(),
                        backend_slot: slot.to_owned(),
                    }
                    .to_request_body(),
                    Err(SnapdError::Transport { .. })
                ),
                "{slot:?} should be rejected"
            );
        }
    }

    #[test]
    fn slot_name_validation_matches_grammar() {
        assert!(is_valid_slot_name("provider"));
        assert!(is_valid_slot_name("a"));
        assert!(is_valid_slot_name("provider-2"));
        assert!(!is_valid_slot_name(""));
        assert!(!is_valid_slot_name("2provider"));
        assert!(!is_valid_slot_name("provider-"));
        assert!(!is_valid_slot_name("pro--vider"));
        assert!(!is_valid_slot_name("pro_vider"));
    }

    #[test]
    fn interface_action_rejects_invalid_snap_name() {
        assert!(matches!(
            InterfaceAction::Connect {
                backend_snap: "bad;name".to_owned(),
                backend_slot: "provider".to_owned(),
            }
            .to_request_body(),
            Err(SnapdError::Transport { .. })
        ));
    }

    #[test]
    fn parse_envelope_supports_sync_async_error() {
        let sync =
            parse_envelope(r#"{"type":"sync","status-code":200,"result":{"ok":true}}"#).unwrap();
        assert!(matches!(sync, Envelope::Sync { .. }));

        let async_env =
            parse_envelope(r#"{"type":"async","status-code":202,"change":"42"}"#).unwrap();
        match async_env {
            Envelope::Async { change_id } => {
                assert_eq!(change_id, "42");
            }
            _ => panic!("expected async"),
        }

        let error =
            parse_envelope(r#"{"type":"error","status-code":401,"result":{"message":"nope","kind":"login-required"}}"#).unwrap();
        match error {
            Envelope::Error {
                status_code,
                kind,
                message,
            } => {
                assert_eq!(status_code, 401);
                assert_eq!(kind.as_deref(), Some("login-required"));
                assert_eq!(message, "nope");
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn classify_error_maps_auth_denials() {
        let denied = classify_error(401, None, "access denied".into());
        assert!(matches!(denied, SnapdError::AuthorizationDenied { .. }));
        let forbidden = classify_error(403, None, "forbidden".into());
        assert!(matches!(forbidden, SnapdError::AuthorizationDenied { .. }));
        let generic = classify_error(400, None, "bad".into());
        assert!(matches!(generic, SnapdError::Snapd { .. }));
    }

    #[test]
    fn dismissing_the_authorization_prompt_is_a_cancel() {
        let dismissed = classify_error(403, Some("auth-cancelled".into()), "cancelled".into());
        assert_eq!(dismissed, SnapdError::Cancelled);
    }

    #[test]
    fn parse_headers_reads_status_and_content_length() {
        let headers = parse_headers(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12",
        )
        .unwrap();
        assert_eq!(headers.status_code, 200);
        assert_eq!(headers.content_length, Some(12));
        assert!(!headers.chunked);
    }

    #[test]
    fn parse_headers_reads_chunked_transfer_encoding() {
        let headers = parse_headers("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked").unwrap();
        assert_eq!(headers.status_code, 200);
        assert!(headers.chunked);
        assert_eq!(headers.content_length, None);
    }

    #[test]
    fn parse_headers_rejects_conflicting_content_length_values_in_one_header() {
        assert!(matches!(
            parse_headers("HTTP/1.1 200 OK\r\nContent-Length: 5, 6"),
            Err(SnapdError::Protocol { .. })
        ));
    }

    #[test]
    fn parse_headers_accepts_agreeing_duplicate_content_length() {
        let headers =
            parse_headers("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5").unwrap();
        assert_eq!(headers.content_length, Some(5));
    }

    #[test]
    fn parse_headers_rejects_conflicting_duplicate_content_length_headers() {
        assert!(matches!(
            parse_headers("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6"),
            Err(SnapdError::Protocol { .. })
        ));
    }

    #[test]
    fn parse_headers_rejects_unsupported_transfer_encoding() {
        for value in ["gzip", "identity", "chunked, gzip", "gzip, chunked"] {
            let text = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: {value}");
            assert!(
                matches!(parse_headers(&text), Err(SnapdError::Protocol { .. })),
                "expected rejection for Transfer-Encoding: {value:?}"
            );
        }
    }

    #[test]
    fn parse_headers_rejects_both_content_length_and_transfer_encoding() {
        assert!(matches!(
            parse_headers("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked"),
            Err(SnapdError::Protocol { .. })
        ));
    }

    #[test]
    fn is_valid_change_id_rejects_paths_and_control_bytes() {
        assert!(is_valid_change_id("42"));
        assert!(is_valid_change_id("Abc-123_xyz"));
        assert!(!is_valid_change_id(""));
        assert!(!is_valid_change_id("../secrets"));
        assert!(!is_valid_change_id("42/interfaces"));
        assert!(!is_valid_change_id("42\r\nX-Injected: yes"));
        assert!(!is_valid_change_id("42?foo=bar"));
        assert!(!is_valid_change_id("has space"));
        assert!(!is_valid_change_id(&"a".repeat(129)));
    }

    #[test]
    fn decode_chunked_rejects_missing_trailing_crlf_after_chunk_data() {
        // "5\r\nhello__" — chunk of length 5, followed by garbage instead of CRLF.
        let mut bytes = Vec::from("5\r\nhelloXY\r\n0\r\n\r\n".as_bytes());
        let outcome = decode_chunked(&mut bytes);
        assert!(matches!(outcome, Err(SnapdError::Protocol { .. })));
    }

    #[test]
    fn decode_chunked_accepts_a_zero_terminated_stream() {
        let mut bytes = Vec::from("5\r\nhello\r\n0\r\n\r\n".as_bytes());
        let first = decode_chunked(&mut bytes).unwrap();
        assert!(matches!(first, ChunkStep::Chunk(ref chunk) if chunk == b"hello"));
        // After draining the first chunk we should see the zero-chunk end.
        // The trailing "\r\n" after the zero chunk is required.
        let second = decode_chunked(&mut bytes).unwrap();
        assert!(matches!(second, ChunkStep::Complete));
    }

    #[test]
    fn decode_chunked_rejects_non_hex_chunk_length() {
        let mut bytes = Vec::from("+5\r\nhello\r\n0\r\n\r\n".as_bytes());
        assert!(matches!(
            decode_chunked(&mut bytes),
            Err(SnapdError::Protocol { .. })
        ));
    }
}
