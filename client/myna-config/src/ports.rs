use thiserror::Error;

use async_trait::async_trait;

use crate::active_backend::SwitchPlan;
use crate::backend_apply::ApplyPreview;
use crate::command::CancellationToken;
use crate::diagnostics::InstalledSnap;
use crate::domain::{
    BackendIdentity, BackendSnapshot, BackendSurfaceError, ClientSetting, ClientSettingMetadata,
    ClientSettingValue, CommandResult, ConnectionSnapshot,
};
use crate::onboarding::ExtensionState;
use crate::snap_changes::{apply_progress, ApplyProgress, ChangeInProgress};

pub type ClientSettingsCallback = Box<dyn Fn(ClientSetting) + 'static>;

pub trait ClientSettingsSubscription {}

pub trait ClientSettings {
    fn list(&self) -> Result<Vec<ClientSettingMetadata>, ClientSettingsError>;
    fn get(&self, key: &str) -> Result<ClientSettingValue, ClientSettingsError>;
    fn set(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError>;
    /// Whether the store holds a value for `key`, rather than the schema
    /// default standing in for one.
    fn has_user_value(&self, key: &str) -> Result<bool, ClientSettingsError>;
    fn subscribe(
        &self,
        callback: ClientSettingsCallback,
    ) -> Result<Box<dyn ClientSettingsSubscription>, ClientSettingsError>;
}

#[async_trait(?Send)]
pub trait BackendRepository {
    async fn installed_snaps(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<InstalledSnap>, BackendSurfaceError> {
        Ok(Vec::new())
    }

    async fn discover(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError>;

    async fn read_snapshot(
        &self,
        backend: &BackendIdentity,
        cancellation: CancellationToken,
    ) -> BackendSnapshot;

    async fn refresh(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError>;
}

#[async_trait(?Send)]
pub trait SystemConfigurator {
    async fn execute_backend_switch(
        &self,
        plan: &SwitchPlan,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure>;

    /// Restart Myna's user service so it picks up a changed backend mount.
    async fn restart_myna(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError>;

    async fn apply_backend_config(
        &self,
        preview: &ApplyPreview,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure>;

    /// The snapd changes that have not finished yet, read as the user.
    async fn changes_in_progress(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ChangeInProgress>, String> {
        Ok(Vec::new())
    }

    /// Whether snapd's `experimental.user-daemons` flag is on, read as the
    /// user.
    async fn user_daemons_enabled(&self, cancellation: CancellationToken) -> Result<bool, String>;

    /// Run `plan` as root under one `pkexec`: snapd keeps a polkit
    /// authorization per action, and the flag, installs and connect are
    /// three, so asking snapd as the user could prompt three times. Returns
    /// once every step is done; the installs' changes can be read meanwhile.
    async fn set_up(
        &self,
        plan: &SetUpPlan,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError>;

    /// Start installing `snap` from edge, as the user: snapd raises polkit's
    /// prompt itself. The change it started, none when it is installed
    /// already.
    async fn install_snap(
        &self,
        snap: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<String>, SystemConfiguratorError>;

    /// One snapd change as it stands now, read as the user.
    async fn snap_change(
        &self,
        change_id: &str,
        cancellation: CancellationToken,
    ) -> Result<ChangeInProgress, String>;

    /// What snapd is doing on `backend_snap` now, while an apply runs; none
    /// when it is doing nothing there or cannot be read.
    async fn apply_progress(
        &self,
        backend_snap: &str,
        cancellation: CancellationToken,
    ) -> Option<ApplyProgress> {
        let changes = self.changes_in_progress(cancellation).await.ok()?;
        apply_progress(&changes, backend_snap)
    }
}

/// What one privileged set-up does, in order: turn snapd's
/// `experimental.user-daemons` flag on, install each snap from edge, then
/// connect `myna:backend` to a model snap's slot. The connect does not wait
/// on snapd's auto-connect, which a bare machine was seen to skip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetUpPlan {
    pub flag: bool,
    pub installs: Vec<&'static str>,
    pub connect: Option<&'static str>,
}

/// GNOME Shell's extensions, as the running shell reports them.
#[async_trait(?Send)]
pub trait ShellExtensions {
    /// Where `uuid` stands. A session with no gnome-shell has none.
    async fn extension_state(&self, uuid: &str) -> ExtensionState;

    /// Have gnome-shell enable `uuid`, and wait until it runs it. Needs no
    /// authorization: it is the user's own shell.
    async fn enable_extension(&self, uuid: &str) -> Result<(), SystemConfiguratorError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemConfiguratorFailure {
    completed: Vec<CommandResult>,
    error: SystemConfiguratorError,
}

impl SystemConfiguratorFailure {
    pub fn new(completed: Vec<CommandResult>, error: SystemConfiguratorError) -> Self {
        Self { completed, error }
    }

    pub fn completed(&self) -> &[CommandResult] {
        &self.completed
    }

    pub fn error(&self) -> &SystemConfiguratorError {
        &self.error
    }

    pub fn into_parts(self) -> (Vec<CommandResult>, SystemConfiguratorError) {
        (self.completed, self.error)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ClientSettingsError {
    #[error("GSettings schema {schema_id} is unavailable. {guidance}")]
    SchemaUnavailable {
        schema_id: &'static str,
        guidance: &'static str,
    },
    #[error("settings key is not declared by the schema: {key}")]
    UnknownKey { key: String },
    #[error("settings key is not writable: {key}")]
    NotWritable { key: String },
    #[error("invalid value for {key}: {message}")]
    InvalidValue { key: String, message: String },
    #[error("cannot open the Myna settings store: {message}")]
    StoreUnavailable { message: String },
}

/// The privileged step a failure report names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailedStep {
    /// A process run directly or under `pkexec`.
    Command {
        executable: String,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: String,
    },
    /// A request to snapd's REST API made as the user, such as
    /// `POST /v2/snaps/myna (install, latest/edge)`; no
    /// status when snapd never answered.
    Snapd {
        request: String,
        http_status: Option<u16>,
    },
    /// A method call on the session bus, such as
    /// `org.gnome.Shell.Extensions.EnableExtension("myna-shell@canonical.com")`.
    DBus { call: String },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SystemConfiguratorError {
    #[error("privileged configuration was cancelled")]
    Cancelled,
    #[error("authorization denied: {message}")]
    AuthorizationDenied { step: FailedStep, message: String },
    #[error("the model rejected the requested values: {message}")]
    ValuesRejected { step: FailedStep, message: String },
    #[error("privileged configuration failed: {message}")]
    Execution { step: FailedStep, message: String },
}

impl SystemConfiguratorError {
    pub fn authorization_denied(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
    ) -> Self {
        let (step, message) = command_failure(
            executable,
            arguments,
            exit_status,
            stderr,
            "authorization denied",
        );
        Self::AuthorizationDenied { step, message }
    }

    pub fn values_rejected(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
    ) -> Self {
        let (step, message) = command_failure(
            executable,
            arguments,
            exit_status,
            stderr,
            "the model rejected the requested values",
        );
        Self::ValuesRejected { step, message }
    }

    pub fn execution(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Execution {
            step: FailedStep::Command {
                executable: executable.into(),
                arguments,
                exit_status,
                stderr: stderr.into(),
            },
            message: message.into(),
        }
    }

    pub fn snapd_authorization_denied(
        request: impl Into<String>,
        http_status: u16,
        message: impl Into<String>,
    ) -> Self {
        Self::AuthorizationDenied {
            step: FailedStep::Snapd {
                request: request.into(),
                http_status: Some(http_status),
            },
            message: message.into(),
        }
    }

    pub fn snapd_execution(
        request: impl Into<String>,
        http_status: Option<u16>,
        message: impl Into<String>,
    ) -> Self {
        Self::Execution {
            step: FailedStep::Snapd {
                request: request.into(),
                http_status,
            },
            message: message.into(),
        }
    }

    pub fn dbus_execution(call: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Execution {
            step: FailedStep::DBus { call: call.into() },
            message: message.into(),
        }
    }

    /// The step that failed; none for a cancellation.
    pub fn step(&self) -> Option<&FailedStep> {
        match self {
            Self::Cancelled => None,
            Self::AuthorizationDenied { step, .. }
            | Self::ValuesRejected { step, .. }
            | Self::Execution { step, .. } => Some(step),
        }
    }
}

/// A failed command whose message is its stderr, or `fallback` when it
/// printed nothing.
fn command_failure(
    executable: impl Into<String>,
    arguments: Vec<String>,
    exit_status: Option<i32>,
    stderr: impl Into<String>,
    fallback: &str,
) -> (FailedStep, String) {
    let stderr = stderr.into();
    let message = if stderr.trim().is_empty() {
        fallback.to_owned()
    } else {
        stderr.trim().to_owned()
    };
    (
        FailedStep::Command {
            executable: executable.into(),
            arguments,
            exit_status,
            stderr,
        },
        message,
    )
}
