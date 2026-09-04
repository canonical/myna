use thiserror::Error;

use async_trait::async_trait;

use crate::active_backend::SwitchPlan;
use crate::backend_apply::ApplyPreview;
use crate::command::{CancellationToken, CommandRequest};
use crate::diagnostics::InstalledSnap;
use crate::domain::{
    BackendIdentity, BackendSnapshot, BackendSurfaceError, ClientSetting, ClientSettingMetadata,
    ClientSettingValue, CommandResult, ConnectionSnapshot,
};

pub type ClientSettingsCallback = Box<dyn Fn(ClientSetting) + 'static>;

pub trait ClientSettingsSubscription {}

pub trait ClientSettings {
    fn list(&self) -> Result<Vec<ClientSettingMetadata>, ClientSettingsError>;
    fn get(&self, key: &str) -> Result<ClientSettingValue, ClientSettingsError>;
    fn set(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError>;
    fn reset(&self, key: &str) -> Result<(), ClientSettingsError>;
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
    async fn execute_privileged(
        &self,
        _operations: &[CommandRequest],
        _cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        Err(SystemConfiguratorFailure::new(
            Vec::new(),
            SystemConfiguratorError::execution(
                "pkexec",
                Vec::new(),
                None,
                "",
                "generic privileged operations are not supported",
            ),
        ))
    }

    async fn execute_backend_switch(
        &self,
        plan: &SwitchPlan,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        self.execute_privileged(plan.operations(), cancellation)
            .await
    }

    async fn apply_backend_config(
        &self,
        preview: &ApplyPreview,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        self.execute_privileged(preview.operations(), cancellation)
            .await
    }
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SystemConfiguratorError {
    #[error("privileged configuration was cancelled")]
    Cancelled,
    #[error("authorization denied: {message}")]
    AuthorizationDenied {
        executable: String,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: String,
        message: String,
    },
    #[error("the backend rejected the requested values: {message}")]
    ValuesRejected {
        executable: String,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: String,
        message: String,
    },
    #[error("privileged configuration failed: {message}")]
    Execution {
        executable: String,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: String,
        message: String,
    },
}

impl SystemConfiguratorError {
    pub fn authorization_denied(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
    ) -> Self {
        let stderr = stderr.into();
        Self::AuthorizationDenied {
            executable: executable.into(),
            arguments,
            exit_status,
            message: if stderr.trim().is_empty() {
                "authorization denied".to_owned()
            } else {
                stderr.trim().to_owned()
            },
            stderr,
        }
    }

    pub fn values_rejected(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
    ) -> Self {
        let stderr = stderr.into();
        Self::ValuesRejected {
            executable: executable.into(),
            arguments,
            exit_status,
            message: if stderr.trim().is_empty() {
                "the backend rejected the requested values".to_owned()
            } else {
                stderr.trim().to_owned()
            },
            stderr,
        }
    }

    pub fn execution(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stderr: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Execution {
            executable: executable.into(),
            arguments,
            exit_status,
            stderr: stderr.into(),
            message: message.into(),
        }
    }
}
