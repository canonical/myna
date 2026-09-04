use std::collections::{BTreeMap, VecDeque};
use std::future::{poll_fn, Future};
use std::io;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::task::{Poll, Waker};
use std::time::Duration;

use async_trait::async_trait;
use gio::{glib, SubprocessFlags, SubprocessLauncher};
use glib::translate::*;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRequest {
    executable: String,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    timeout: Duration,
}

impl CommandRequest {
    pub fn new(executable: String, arguments: Vec<String>) -> Self {
        Self {
            executable,
            arguments,
            environment: BTreeMap::new(),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    exit_status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    pub fn new(
        exit_status: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            exit_status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    pub fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("command not found: {executable}")]
    NotFound { executable: String },
    #[error("could not spawn {executable}: {message}")]
    Spawn {
        executable: String,
        kind: io::ErrorKind,
        message: String,
    },
    #[error("command timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("command was cancelled")]
    Cancelled,
    #[error("command exited unsuccessfully with status {exit_status:?}")]
    NonZero {
        exit_status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("{stream:?} was not valid UTF-8: {message}")]
    InvalidUtf8 {
        stream: OutputStream,
        message: String,
    },
    #[error("fake command runner has no scripted outcome")]
    FakeScriptExhausted,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            let wakers = std::mem::take(
                &mut *self
                    .inner
                    .wakers
                    .lock()
                    .expect("cancellation token lock poisoned"),
            );
            for waker in wakers {
                waker.wake();
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    fn poll_cancelled(&self, waker: &Waker) -> bool {
        if self.is_cancelled() {
            return true;
        }

        let mut wakers = self
            .inner
            .wakers
            .lock()
            .expect("cancellation token lock poisoned");
        if self.is_cancelled() {
            return true;
        }
        if !wakers.iter().any(|registered| registered.will_wake(waker)) {
            wakers.push(waker.clone());
        }
        false
    }
}

#[async_trait(?Send)]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, CommandError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GioCommandRunner;

pub use GioCommandRunner as TokioCommandRunner;

#[async_trait(?Send)]
impl CommandRunner for GioCommandRunner {
    async fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, CommandError> {
        if cancellation.is_cancelled() {
            return Err(CommandError::Cancelled);
        }

        let launcher =
            SubprocessLauncher::new(SubprocessFlags::STDOUT_PIPE | SubprocessFlags::STDERR_PIPE);
        for (name, value) in &request.environment {
            launcher.setenv(name, value, true);
        }
        let argv = std::iter::once(request.executable.as_str())
            .chain(request.arguments.iter().map(String::as_str))
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>();
        let subprocess = launcher
            .spawn(&argv)
            .map_err(|error| spawn_error(&request.executable, error))?;

        let mut communication = subprocess.communicate_future(None);
        let mut timeout = glib::timeout_future(request.timeout);
        let result = poll_fn(|context| {
            if cancellation.poll_cancelled(context.waker()) {
                return Poll::Ready(ProcessResult::Cancelled);
            }
            if let Poll::Ready(result) = communication.as_mut().poll(context) {
                return Poll::Ready(ProcessResult::Completed(result));
            }
            if Pin::new(&mut timeout).poll(context).is_ready() {
                return Poll::Ready(ProcessResult::TimedOut);
            }
            Poll::Pending
        })
        .await;

        let (stdout, stderr) = match result {
            ProcessResult::Completed(result) => {
                result.map_err(|error| spawn_error(&request.executable, error))?
            }
            ProcessResult::Cancelled => {
                subprocess.force_exit();
                return Err(CommandError::Cancelled);
            }
            ProcessResult::TimedOut => {
                subprocess.force_exit();
                return Err(CommandError::Timeout {
                    timeout: request.timeout,
                });
            }
        };
        let stdout = String::from_utf8(stdout.map_or_else(Vec::new, |bytes| bytes.to_vec()))
            .map_err(|error| CommandError::InvalidUtf8 {
                stream: OutputStream::Stdout,
                message: error.to_string(),
            })?;
        let stderr = String::from_utf8(stderr.map_or_else(Vec::new, |bytes| bytes.to_vec()))
            .map_err(|error| CommandError::InvalidUtf8 {
                stream: OutputStream::Stderr,
                message: error.to_string(),
            })?;
        let exit_status = subprocess.has_exited().then(|| subprocess.exit_status());

        if !subprocess.is_successful() {
            return Err(CommandError::NonZero {
                exit_status,
                stdout,
                stderr,
            });
        }

        Ok(CommandOutput::new(exit_status, stdout, stderr))
    }
}

enum ProcessResult {
    Completed(Result<(Option<glib::Bytes>, Option<glib::Bytes>), glib::Error>),
    TimedOut,
    Cancelled,
}

fn spawn_error(executable: &str, error: glib::Error) -> CommandError {
    let kind = glib_error_kind(&error);
    if kind == io::ErrorKind::NotFound {
        CommandError::NotFound {
            executable: executable.to_owned(),
        }
    } else {
        CommandError::Spawn {
            executable: executable.to_owned(),
            kind,
            message: error.to_string(),
        }
    }
}

fn glib_error_kind(error: &glib::Error) -> io::ErrorKind {
    if let Some(kind) = error.kind::<gio::IOErrorEnum>() {
        return kind.into();
    }

    let spawn_error_domain = unsafe { from_glib(glib::ffi::g_spawn_error_quark()) };
    if error.domain() != spawn_error_domain {
        return io::ErrorKind::Other;
    }
    match error.code() {
        glib::ffi::G_SPAWN_ERROR_NOENT => io::ErrorKind::NotFound,
        glib::ffi::G_SPAWN_ERROR_ACCES | glib::ffi::G_SPAWN_ERROR_PERM => {
            io::ErrorKind::PermissionDenied
        }
        glib::ffi::G_SPAWN_ERROR_INVAL => io::ErrorKind::InvalidInput,
        _ => io::ErrorKind::Other,
    }
}

#[derive(Clone, Default)]
pub struct FakeCommandRunner {
    inner: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    calls: Vec<CommandRequest>,
    outcomes: VecDeque<Result<CommandOutput, CommandError>>,
}

impl FakeCommandRunner {
    pub fn scripted(
        outcomes: impl IntoIterator<Item = Result<CommandOutput, CommandError>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                calls: Vec::new(),
                outcomes: outcomes.into_iter().collect(),
            })),
        }
    }

    pub fn calls(&self) -> Vec<CommandRequest> {
        self.inner
            .lock()
            .expect("fake runner lock poisoned")
            .calls
            .clone()
    }
}

#[async_trait(?Send)]
impl CommandRunner for FakeCommandRunner {
    async fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, CommandError> {
        let mut state = self.inner.lock().expect("fake runner lock poisoned");
        state.calls.push(request);
        if cancellation.is_cancelled() {
            return Err(CommandError::Cancelled);
        }
        state
            .outcomes
            .pop_front()
            .unwrap_or(Err(CommandError::FakeScriptExhausted))
    }
}
