use std::time::Duration;

use crate::backend_controller::BackendPage;
use crate::command::{CancellationToken, CommandRequest};
use crate::domain::{
    BackendIdentity, BackendSnapshot, BackendSurface, CommandResult, ConfigValue, ServiceState,
    StagedChange,
};
use crate::ports::{BackendRepository, FailedStep, SystemConfigurator, SystemConfiguratorError};

const READINESS_ATTEMPTS: usize = 30;
#[cfg(not(test))]
const READINESS_RETRY_DELAY: Duration = Duration::from_secs(1);
#[cfg(test)]
const READINESS_RETRY_DELAY: Duration = Duration::ZERO;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    title: String,
    message: String,
}

impl ValidationIssue {
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareApplyError {
    NoChanges,
    Invalid(Vec<ValidationIssue>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApplyPreview {
    backend: BackendIdentity,
    changes: Vec<StagedChange>,
    operations: Vec<CommandRequest>,
}

impl ApplyPreview {
    pub fn new(
        backend: BackendIdentity,
        changes: Vec<StagedChange>,
    ) -> Result<Self, PrepareApplyError> {
        if changes.is_empty() {
            return Err(PrepareApplyError::NoChanges);
        }

        let mut changes = changes;
        changes.sort_by(|left, right| left.key().cmp(right.key()));

        let mut issues = validate_backend_identity(&backend);
        let mut assignments = Vec::new();
        let mut model = None;
        let mut engine = None;
        for change in &changes {
            match change.key() {
                "model" | "engine" => match change.proposed() {
                    ConfigValue::Text(value) => {
                        if change.key() == "model" {
                            model = Some(value.clone());
                        } else {
                            engine = Some(value.clone());
                        }
                    }
                    _ => issues.push(ValidationIssue::new(
                        change.key(),
                        gettextrs::gettext("selector value must be text"),
                    )),
                },
                _ => match serialize_assignment(change.key(), change.proposed()) {
                    Ok(argument) => assignments.push(argument),
                    Err(message) => issues.push(ValidationIssue::new(change.key(), message)),
                },
            }
        }
        let modelctl_app = backend.modelctl_app().filter(|app| !app.trim().is_empty());
        if modelctl_app.is_none() {
            for change in &changes {
                issues.push(ValidationIssue::new(
                    change.key(),
                    gettextrs::gettext("the model's settings command could not be found"),
                ));
            }
        }
        if !issues.is_empty() {
            return Err(PrepareApplyError::Invalid(issues));
        }

        let mut operations = Vec::new();
        if let Some(app) = modelctl_app {
            if !assignments.is_empty() {
                operations.push(modelctl_set_operation(app, assignments));
            }
            if let Some(model) = model {
                operations.push(modelctl_operation(app, "use-model", Some(model)));
            }
            if let Some(engine) = engine {
                operations.push(if engine == "auto" {
                    modelctl_operation(app, "use-engine", None)
                } else {
                    modelctl_operation(app, "use-engine", Some(engine))
                });
            }
        }
        operations.push(CommandRequest::new(
            "snap".to_owned(),
            vec!["restart".to_owned(), backend.snap_name().to_owned()],
        ));
        Ok(Self {
            backend,
            changes,
            operations,
        })
    }

    pub fn backend(&self) -> &BackendIdentity {
        &self.backend
    }

    pub fn changes(&self) -> &[StagedChange] {
        &self.changes
    }

    pub fn operations(&self) -> &[CommandRequest] {
        &self.operations
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadBackMismatch {
    key: String,
    requested: ConfigValue,
    actual: Option<ConfigValue>,
}

impl ReadBackMismatch {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn requested(&self) -> &ConfigValue {
        &self.requested
    }

    pub fn actual(&self) -> Option<&ConfigValue> {
        self.actual.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegedFailure {
    step: FailedStep,
    message: String,
}

impl PrivilegedFailure {
    pub fn step(&self) -> &FailedStep {
        &self.step
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplyFailure {
    CancelledExecution,
    VerificationCancelled {
        commands: Vec<CommandResult>,
        snapshot: Box<BackendSnapshot>,
    },
    AuthorizationDenied {
        details: PrivilegedFailure,
    },
    ValuesRejected {
        details: PrivilegedFailure,
    },
    Execution {
        details: PrivilegedFailure,
    },
    PartialExecution {
        snapshot: Box<BackendSnapshot>,
        commands: Vec<CommandResult>,
        failure: Box<SystemConfiguratorError>,
    },
    RestartReadiness {
        snapshot: Box<BackendSnapshot>,
        message: String,
    },
    ReadBackUnavailable {
        snapshot: Box<BackendSnapshot>,
        errors: Vec<crate::domain::BackendSurfaceError>,
    },
    ReadBackMismatch {
        snapshot: Box<BackendSnapshot>,
        mismatches: Vec<ReadBackMismatch>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApplySuccess {
    snapshot: BackendSnapshot,
}

impl ApplySuccess {
    pub fn snapshot(&self) -> &BackendSnapshot {
        &self.snapshot
    }
}

/// The plan for the one change the user just made on the Model tab: `key`
/// becomes `value`, replacing what the page last read.
pub fn prepare_change(
    page: &BackendPage,
    key: &str,
    value: ConfigValue,
) -> Result<ApplyPreview, PrepareApplyError> {
    let Some(row) = page.rows().iter().find(|row| row.key() == key) else {
        return Err(PrepareApplyError::NoChanges);
    };
    if row.value() == &value {
        return Err(PrepareApplyError::NoChanges);
    }
    let metadata = row.metadata();
    let invalid = |message: String| {
        PrepareApplyError::Invalid(vec![ValidationIssue::new(metadata.title(), message)])
    };
    metadata.validation().validate(&value).map_err(invalid)?;
    let change = StagedChange::new(key, row.value().clone(), value)
        .map_err(|error| invalid(error.message().to_owned()))?;
    let identity = page
        .snapshot()
        .map(|snapshot| snapshot.identity().clone())
        .unwrap_or_else(|| page.identity().clone());
    ApplyPreview::new(identity, vec![change])
}

pub async fn execute_backend_apply(
    preview: &ApplyPreview,
    configurator: &dyn SystemConfigurator,
    repository: &dyn BackendRepository,
    cancellation: CancellationToken,
) -> Result<ApplySuccess, ApplyFailure> {
    let commands = match configurator
        .apply_backend_config(preview, cancellation.clone())
        .await
    {
        Ok(commands) => commands,
        Err(failure) => {
            let (commands, error) = failure.into_parts();
            if commands.is_empty() {
                return Err(map_system_error(error));
            }
            let snapshot = repository
                .read_snapshot(preview.backend(), CancellationToken::new())
                .await;
            return Err(ApplyFailure::PartialExecution {
                snapshot: Box::new(snapshot),
                commands,
                failure: Box::new(error),
            });
        }
    };
    let invalid_results = commands.len() != preview.operations().len()
        || commands
            .iter()
            .zip(preview.operations())
            .any(|(result, request)| {
                result.executable() != request.executable()
                    || result.arguments() != request.arguments()
                    || result.exit_status() != Some(0)
            });
    if invalid_results {
        let error = SystemConfiguratorError::execution(
            "pkexec",
            Vec::new(),
            None,
            "",
            "the privileged apply plan did not execute every operation",
        );
        if commands.is_empty() {
            return Err(map_system_error(error));
        }
        let snapshot = repository
            .read_snapshot(preview.backend(), CancellationToken::new())
            .await;
        return Err(ApplyFailure::PartialExecution {
            snapshot: Box::new(snapshot),
            commands,
            failure: Box::new(error),
        });
    }

    if cancellation.is_cancelled() {
        let snapshot = repository
            .read_snapshot(preview.backend(), CancellationToken::new())
            .await;
        return Err(ApplyFailure::PartialExecution {
            snapshot: Box::new(snapshot),
            commands,
            failure: Box::new(SystemConfiguratorError::Cancelled),
        });
    }

    let mut attempt = 0;
    let verification = CancellationToken::new();
    let snapshot = loop {
        let snapshot = repository
            .read_snapshot(preview.backend(), verification.clone())
            .await;

        if verification.is_cancelled() {
            return Err(ApplyFailure::VerificationCancelled {
                commands,
                snapshot: Box::new(snapshot),
            });
        }
        if cancellation.is_cancelled() {
            return Err(ApplyFailure::PartialExecution {
                snapshot: Box::new(snapshot),
                commands,
                failure: Box::new(SystemConfiguratorError::Cancelled),
            });
        }

        if let Some(message) = readiness_failure(&snapshot) {
            attempt += 1;
            if attempt >= READINESS_ATTEMPTS || readiness_failure_is_terminal(&snapshot) {
                return Err(ApplyFailure::RestartReadiness {
                    snapshot: Box::new(snapshot),
                    message,
                });
            }
        } else {
            let read_errors = read_back_errors(preview, &snapshot);
            if read_errors.is_empty() {
                break snapshot;
            }
            attempt += 1;
            if attempt >= READINESS_ATTEMPTS {
                return Err(ApplyFailure::ReadBackUnavailable {
                    snapshot: Box::new(snapshot),
                    errors: read_errors,
                });
            }
        }
        gio::glib::timeout_future(READINESS_RETRY_DELAY).await;
    };

    let mismatches = read_back_mismatches(preview, &snapshot);
    if !mismatches.is_empty() {
        return Err(ApplyFailure::ReadBackMismatch {
            snapshot: Box::new(snapshot),
            mismatches,
        });
    }

    Ok(ApplySuccess { snapshot })
}

fn validate_backend_identity(backend: &BackendIdentity) -> Vec<ValidationIssue> {
    if backend.snap_name().trim().is_empty() {
        vec![ValidationIssue::new(
            gettextrs::gettext("Model"),
            gettextrs::gettext("the model's restart command could not be built"),
        )]
    } else {
        Vec::new()
    }
}

/// `modelctl set` writes the user layer of the backend's configuration, which
/// is the layer `modelctl get` reads back without privileges. The restart is
/// a separate, explicit operation so the plan stays inspectable.
fn modelctl_set_operation(app: &str, assignments: Vec<String>) -> CommandRequest {
    let mut arguments = vec!["run".to_owned(), app.to_owned(), "set".to_owned()];
    arguments.extend(assignments);
    arguments.extend(["--assume-yes".to_owned(), "--no-restart".to_owned()]);
    CommandRequest::new("snap".to_owned(), arguments)
}

fn modelctl_operation(app: &str, selector: &str, value: Option<String>) -> CommandRequest {
    let mut arguments = vec!["run".to_owned(), app.to_owned(), selector.to_owned()];
    match value {
        Some(value) => arguments.push(value),
        None => arguments.push("--auto".to_owned()),
    }
    arguments.extend(["--assume-yes".to_owned(), "--no-restart".to_owned()]);
    CommandRequest::new("snap".to_owned(), arguments)
}

fn read_back_errors(
    preview: &ApplyPreview,
    snapshot: &BackendSnapshot,
) -> Vec<crate::domain::BackendSurfaceError> {
    let mut surfaces = Vec::new();
    for change in preview.changes() {
        let surface = match change.key() {
            "model" => BackendSurface::Models,
            "engine" => BackendSurface::Engines,
            _ => BackendSurface::ModelctlConfig,
        };
        if !surfaces.contains(&surface) {
            surfaces.push(surface);
        }
    }
    surfaces
        .into_iter()
        .filter_map(|surface| snapshot.error(surface).cloned())
        .collect()
}

pub fn read_back_mismatches(
    preview: &ApplyPreview,
    snapshot: &BackendSnapshot,
) -> Vec<ReadBackMismatch> {
    preview
        .changes()
        .iter()
        .filter_map(|change| {
            let actual = snapshot.value(change.key());
            let confirmed = actual.as_ref() == Some(change.proposed())
                || auto_engine_resolved(change, snapshot);
            if confirmed {
                None
            } else {
                Some(ReadBackMismatch {
                    key: change.key().to_owned(),
                    requested: change.proposed().clone(),
                    actual,
                })
            }
        })
        .collect()
}

fn auto_engine_resolved(change: &StagedChange, snapshot: &BackendSnapshot) -> bool {
    if change.key() != "engine" || change.proposed() != &ConfigValue::Text("auto".to_owned()) {
        return false;
    }
    let Some(engines) = snapshot.engines() else {
        return false;
    };
    let Some(active) = engines.active() else {
        return false;
    };
    engines
        .options()
        .iter()
        .any(|engine| engine.name() == active && engine.compatible())
}

fn serialize_assignment(key: &str, value: &ConfigValue) -> Result<String, String> {
    serialize_value(value).map(|serialized| format!("{key}={serialized}"))
}

fn serialize_value(value: &ConfigValue) -> Result<String, String> {
    match value {
        ConfigValue::Null => Ok("null".to_owned()),
        ConfigValue::Boolean(value) => Ok(value.to_string()),
        ConfigValue::Integer(value) => Ok(value.to_string()),
        ConfigValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(|number| number.to_string())
            .ok_or_else(|| gettextrs::gettext("value must be a finite number")),
        ConfigValue::Text(value) => serde_json::to_string(value).map_err(|error| error.to_string()),
    }
}

fn map_system_error(error: SystemConfiguratorError) -> ApplyFailure {
    match error {
        SystemConfiguratorError::Cancelled => ApplyFailure::CancelledExecution,
        SystemConfiguratorError::AuthorizationDenied { step, message } => {
            ApplyFailure::AuthorizationDenied {
                details: PrivilegedFailure { step, message },
            }
        }
        SystemConfiguratorError::ValuesRejected { step, message } => ApplyFailure::ValuesRejected {
            details: PrivilegedFailure { step, message },
        },
        SystemConfiguratorError::Execution { step, message } => ApplyFailure::Execution {
            details: PrivilegedFailure { step, message },
        },
    }
}

/// Why the restart is not confirmed, for the report's details: a sentence,
/// then the raw reason on the next line when there is one.
fn readiness_failure(snapshot: &BackendSnapshot) -> Option<String> {
    let unconfirmed = || gettextrs::gettext("The model's restart could not be confirmed.");

    if let Some(error) = snapshot.error(BackendSurface::Status) {
        return Some(format!("{}\n{}", unconfirmed(), error.message()));
    }

    if let Some(status) = snapshot.status() {
        if status.services().is_empty() {
            return Some(format!("{}\nno service health was reported", unconfirmed()));
        }
        let failing: Vec<String> = status
            .services()
            .iter()
            .filter(|service| !service.is_active())
            .map(|service| {
                format!(
                    "{} ({})",
                    service.name(),
                    service_state_label(service.state())
                )
            })
            .collect();
        if !failing.is_empty() {
            return Some(format!(
                "{}\n{}",
                gettextrs::gettext("The model did not restart."),
                failing.join(", ")
            ));
        }
    } else {
        return Some(unconfirmed());
    }

    None
}

fn readiness_failure_is_terminal(snapshot: &BackendSnapshot) -> bool {
    snapshot.status().is_some_and(|status| {
        status
            .services()
            .iter()
            .any(|service| matches!(service.state(), ServiceState::Failed))
    })
}

fn service_state_label(state: &ServiceState) -> String {
    match state {
        ServiceState::Active => gettextrs::pgettext("service state", "active"),
        ServiceState::Inactive => gettextrs::pgettext("service state", "inactive"),
        ServiceState::Failed => gettextrs::pgettext("service state", "failed"),
        ServiceState::Unknown(_) => gettextrs::pgettext("service state", "unknown"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::adapters::system_configurator::PkexecSystemConfigurator;
    use crate::apply_plan::plan_output;
    use crate::backend_controller::BackendController;
    use crate::command::{CommandError, CommandOutput, FakeCommandRunner};
    use crate::domain::{
        parse_connections, parse_engine_options, parse_model_options, parse_modelctl_config,
    };

    const STATUS: &str = include_str!("../tests/fixtures/modelctl-status.json");
    const MODELS: &str = include_str!("../tests/fixtures/modelctl-list-models.json");
    const ENGINES: &str = include_str!("../tests/fixtures/modelctl-list-engines.json");

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        gio::glib::MainContext::new().block_on(future)
    }

    fn ok(stdout: &str) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput::new(Some(0), stdout, ""))
    }

    fn successful_results(preview: &ApplyPreview) -> Vec<CommandResult> {
        preview
            .operations()
            .iter()
            .map(|operation| {
                CommandResult::new(
                    operation.executable(),
                    operation.arguments().to_vec(),
                    Some(0),
                    "",
                    "",
                )
            })
            .collect()
    }

    fn backend() -> BackendIdentity {
        BackendIdentity::new("myna-parakeet", "provider")
            .with_modelctl_app("myna-parakeet.modelctl")
    }

    fn modelctl_set(assignments: &[&str]) -> Vec<&'static str> {
        let mut argv = vec!["run", "myna-parakeet.modelctl", "set"];
        argv.extend(
            assignments
                .iter()
                .map(|value| -> &'static str { Box::leak(value.to_string().into_boxed_str()) }),
        );
        argv.extend(["--assume-yes", "--no-restart"]);
        argv
    }

    fn page_with_snapshot(snapshot: BackendSnapshot) -> BackendPage {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(
            request,
            Ok(parse_connections(
                "Interface Plug Slot Notes\ncontent[inference-provider] myna:backend myna-parakeet:provider manual\n",
                "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n",
            )
            .unwrap()),
        );
        let request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(request, snapshot);
        controller.page("myna-parakeet").unwrap()
    }

    fn snapshot_with_configuration(config: &str) -> BackendSnapshot {
        let mut snapshot = BackendSnapshot::empty(backend());
        snapshot.set_modelctl_config(parse_modelctl_config(config).unwrap());
        snapshot.set_status(crate::domain::parse_status(STATUS).unwrap());
        snapshot.set_models(parse_model_options(MODELS).unwrap());
        snapshot.set_engines(parse_engine_options(ENGINES).unwrap());
        snapshot
    }

    type ConfiguratorOutcome = Result<Vec<CommandResult>, SystemConfiguratorError>;

    #[derive(Clone)]
    struct FakeConfigurator {
        calls: Rc<RefCell<Vec<ApplyPreview>>>,
        outcomes: Rc<RefCell<VecDeque<ConfiguratorOutcome>>>,
    }

    impl FakeConfigurator {
        fn scripted(outcomes: impl IntoIterator<Item = ConfiguratorOutcome>) -> Self {
            Self {
                calls: Rc::new(RefCell::new(Vec::new())),
                outcomes: Rc::new(RefCell::new(outcomes.into_iter().collect())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    #[async_trait(?Send)]
    impl SystemConfigurator for FakeConfigurator {
        async fn user_daemons_enabled(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<bool, String> {
            Ok(true)
        }

        async fn set_up(
            &self,
            _plan: &crate::ports::SetUpPlan,
            _cancellation: CancellationToken,
        ) -> Result<(), SystemConfiguratorError> {
            unreachable!("the settings window never sets up")
        }

        async fn install_snap(
            &self,
            _snap: &str,
            _cancellation: CancellationToken,
        ) -> Result<Option<String>, crate::ports::SystemConfiguratorError> {
            unreachable!("the settings window never installs a snap")
        }

        async fn snap_change(
            &self,
            _change_id: &str,
            _cancellation: CancellationToken,
        ) -> Result<crate::snap_changes::ChangeInProgress, String> {
            unreachable!("the settings window never follows an install")
        }

        async fn restart_myna(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), crate::ports::SystemConfiguratorError> {
            unreachable!("an apply never restarts Myna")
        }

        async fn execute_backend_switch(
            &self,
            _plan: &crate::active_backend::SwitchPlan,
            _cancellation: CancellationToken,
        ) -> Result<Vec<CommandResult>, crate::ports::SystemConfiguratorFailure> {
            unreachable!("an apply never switches backends")
        }

        async fn apply_backend_config(
            &self,
            preview: &ApplyPreview,
            _cancellation: CancellationToken,
        ) -> Result<Vec<CommandResult>, crate::ports::SystemConfiguratorFailure> {
            self.calls.borrow_mut().push(preview.clone());
            self.outcomes
                .borrow_mut()
                .pop_front()
                .unwrap()
                .map_err(|error| crate::ports::SystemConfiguratorFailure::new(Vec::new(), error))
        }
    }

    #[derive(Clone)]
    struct FakeRepository {
        snapshots: Rc<RefCell<VecDeque<BackendSnapshot>>>,
    }

    #[async_trait(?Send)]
    impl BackendRepository for FakeRepository {
        async fn discover(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            unreachable!()
        }

        async fn read_snapshot(
            &self,
            _backend: &BackendIdentity,
            _cancellation: CancellationToken,
        ) -> BackendSnapshot {
            self.snapshots.borrow_mut().pop_front().unwrap()
        }

        async fn refresh(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            unreachable!()
        }
    }

    struct CancelOnReadRepository {
        snapshot: BackendSnapshot,
    }

    #[async_trait(?Send)]
    impl BackendRepository for CancelOnReadRepository {
        async fn discover(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            unreachable!()
        }

        async fn read_snapshot(
            &self,
            _backend: &BackendIdentity,
            cancellation: CancellationToken,
        ) -> BackendSnapshot {
            cancellation.cancel();
            self.snapshot.clone()
        }

        async fn refresh(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            unreachable!()
        }
    }

    #[test]
    fn no_op_apply_is_rejected_before_authorization() {
        let page = page_with_snapshot(snapshot_with_configuration(
            "sleep-idle-seconds: 30\nverbose: false\n",
        ));

        for (key, value) in [
            ("sleep-idle-seconds", ConfigValue::Integer(30)),
            ("no-such-setting", ConfigValue::Integer(1)),
        ] {
            let error = prepare_change(&page, key, value).unwrap_err();
            assert!(matches!(error, PrepareApplyError::NoChanges), "{key}");
        }
    }

    #[test]
    fn changing_the_idle_timeout_restarts_the_backend() {
        let page = page_with_snapshot(snapshot_with_configuration("sleep-idle-seconds: 30\n"));

        let preview =
            prepare_change(&page, "sleep-idle-seconds", ConfigValue::Integer(60)).unwrap();

        let operations: Vec<Vec<&str>> = preview
            .operations()
            .iter()
            .map(|operation| operation.arguments().iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            operations,
            [
                modelctl_set(&["sleep-idle-seconds=60"]),
                vec!["restart", "myna-parakeet"],
            ]
        );
    }

    #[test]
    fn preview_serializes_mixed_types_with_stable_safe_single_batch_argv() {
        let preview = ApplyPreview::new(
            BackendIdentity::new("myna-whisper", "provider")
                .with_modelctl_app("myna-whisper.whisper"),
            vec![
                StagedChange::new(
                    "zeta",
                    ConfigValue::Boolean(false),
                    ConfigValue::Boolean(true),
                )
                .unwrap(),
                StagedChange::new(
                    "alpha",
                    ConfigValue::Text("old".into()),
                    ConfigValue::Text(" spaced ; $(rm -rf /) ".into()),
                )
                .unwrap(),
                StagedChange::new("ratio", ConfigValue::Number(0.25), ConfigValue::Number(0.5))
                    .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(preview.operations()[0].executable(), "snap");
        assert_eq!(
            preview.operations()[0].arguments(),
            &[
                "run".to_owned(),
                "myna-whisper.whisper".to_owned(),
                "set".to_owned(),
                r#"alpha=" spaced ; $(rm -rf /) ""#.to_owned(),
                "ratio=0.5".to_owned(),
                "zeta=true".to_owned(),
                "--assume-yes".to_owned(),
                "--no-restart".to_owned(),
            ]
        );
        assert_eq!(
            preview.operations()[1].arguments(),
            &["restart", "myna-whisper"]
        );
    }

    #[test]
    fn preview_groups_snap_settings_then_model_engine_and_final_restart() {
        let preview = ApplyPreview::new(
            backend(),
            vec![
                StagedChange::new(
                    "model",
                    ConfigValue::Text("old-model".into()),
                    ConfigValue::Text("new model; untouched".into()),
                )
                .unwrap(),
                StagedChange::new(
                    "engine",
                    ConfigValue::Text("cpu".into()),
                    ConfigValue::Text("auto".into()),
                )
                .unwrap(),
                StagedChange::new(
                    "verbose",
                    ConfigValue::Boolean(false),
                    ConfigValue::Boolean(true),
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let argv: Vec<Vec<&str>> = preview
            .operations()
            .iter()
            .map(|operation| operation.arguments().iter().map(String::as_str).collect())
            .collect::<Vec<_>>();
        assert_eq!(
            argv,
            vec![
                modelctl_set(&["verbose=true"]),
                vec![
                    "run",
                    "myna-parakeet.modelctl",
                    "use-model",
                    "new model; untouched",
                    "--assume-yes",
                    "--no-restart",
                ],
                vec![
                    "run",
                    "myna-parakeet.modelctl",
                    "use-engine",
                    "--auto",
                    "--assume-yes",
                    "--no-restart",
                ],
                vec!["restart", "myna-parakeet"],
            ]
        );
    }

    #[test]
    fn selector_change_without_resolved_modelctl_is_invalid() {
        let error = ApplyPreview::new(
            BackendIdentity::new("myna-parakeet", "provider"),
            vec![StagedChange::new(
                "model",
                ConfigValue::Text("old".into()),
                ConfigValue::Text("new".into()),
            )
            .unwrap()],
        )
        .unwrap_err();

        assert!(matches!(error, PrepareApplyError::Invalid(_)));
    }

    #[test]
    fn non_auto_engine_uses_explicit_modelctl_value() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "engine",
                ConfigValue::Text("cpu".into()),
                ConfigValue::Text("tensorrt".into()),
            )
            .unwrap()],
        )
        .unwrap();

        assert_eq!(
            preview.operations()[0].arguments(),
            &[
                "run",
                "myna-parakeet.modelctl",
                "use-engine",
                "tensorrt",
                "--assume-yes",
                "--no-restart",
            ]
        );
    }

    #[test]
    fn selector_readback_uses_modelctl_models_and_engines() {
        let preview = ApplyPreview::new(
            backend(),
            vec![
                StagedChange::new(
                    "model",
                    ConfigValue::Text("old".into()),
                    ConfigValue::Text("parakeet-tdt-0.6b-v3".into()),
                )
                .unwrap(),
                StagedChange::new(
                    "engine",
                    ConfigValue::Text("old".into()),
                    ConfigValue::Text("cpu".into()),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let snapshot = snapshot_with_configuration("");

        assert!(read_back_mismatches(&preview, &snapshot).is_empty());
    }

    #[test]
    fn auto_engine_readback_accepts_a_compatible_resolved_engine() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "engine",
                ConfigValue::Text("tensorrt".into()),
                ConfigValue::Text("auto".into()),
            )
            .unwrap()],
        )
        .unwrap();
        let snapshot = snapshot_with_configuration("");

        assert!(read_back_mismatches(&preview, &snapshot).is_empty());
    }

    #[test]
    fn prepared_selector_uses_modelctl_app_resolved_by_snapshot() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(
            request,
            Ok(parse_connections(
                "Interface Plug Slot Notes\ncontent[inference-provider] myna:backend myna-parakeet:provider manual\n",
                "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n",
            )
            .unwrap()),
        );
        let mut snapshot = snapshot_with_configuration("verbose: false\n");
        snapshot.set_identity(backend());
        snapshot.set_models(
            parse_model_options(
                r#"{"active-model":"old","models":[{"name":"old"},{"name":"new"}]}"#,
            )
            .unwrap(),
        );
        let request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(request, snapshot);
        let preview = prepare_change(
            &controller.page("myna-parakeet").unwrap(),
            "model",
            ConfigValue::Text("new".into()),
        )
        .unwrap();

        assert_eq!(
            preview.operations()[0].arguments(),
            &[
                "run",
                "myna-parakeet.modelctl",
                "use-model",
                "new",
                "--assume-yes",
                "--no-restart",
            ]
        );
    }

    #[test]
    fn every_change_ends_in_an_explicit_restart() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();

        assert_eq!(
            preview.operations().last().unwrap().arguments(),
            &["restart", "myna-parakeet"]
        );
    }

    #[test]
    fn invalid_values_are_reported_before_authorization() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(
            request,
            Ok(parse_connections(
                "Interface Plug Slot Notes\ncontent[inference-provider] myna:backend myna-parakeet:provider manual\n",
                "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n",
            )
            .unwrap()),
        );
        let request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(
            request,
            snapshot_with_configuration("sleep-idle-seconds: 30\nverbose: false\n"),
        );
        let error = prepare_change(
            &controller.page("myna-parakeet").unwrap(),
            "sleep-idle-seconds",
            ConfigValue::Text("not-a-number".into()),
        )
        .unwrap_err();

        match error {
            PrepareApplyError::Invalid(issues) => {
                assert_eq!(issues.len(), 1);
                assert_eq!(issues[0].title(), "Unload when idle (seconds)");
                assert_eq!(
                    issues[0].message(),
                    "value must be a non-negative whole number"
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn authorization_denial_is_distinct() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator =
            FakeConfigurator::scripted([Err(SystemConfiguratorError::authorization_denied(
                "pkexec",
                vec![
                    "snap".into(),
                    "set".into(),
                    "myna-parakeet".into(),
                    "verbose=true".into(),
                ],
                Some(126),
                "Not authorized",
            ))]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::new())),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(
            result,
            Err(ApplyFailure::AuthorizationDenied { .. })
        ));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn configure_hook_rejection_is_distinct() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator =
            FakeConfigurator::scripted([Err(SystemConfiguratorError::values_rejected(
                "pkexec",
                vec![
                    "snap".into(),
                    "set".into(),
                    "myna-parakeet".into(),
                    "verbose=true".into(),
                ],
                Some(1),
                "configure hook rejected verbose=true",
            ))]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::new())),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(result, Err(ApplyFailure::ValuesRejected { .. })));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn execution_cancellation_is_distinct() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Err(SystemConfiguratorError::Cancelled)]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::new())),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(result, Err(ApplyFailure::CancelledExecution)));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn cancellation_after_write_is_reported_as_unverified() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let repository = CancelOnReadRepository {
            snapshot: snapshot_with_configuration("verbose: true\n"),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(
            result,
            Err(ApplyFailure::VerificationCancelled { .. })
        ));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn cancellation_observed_after_successful_commands_reconciles_before_reporting() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "verbose: true\n",
            )]))),
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            cancellation,
        ));

        let ApplyFailure::PartialExecution {
            commands, failure, ..
        } = result.unwrap_err()
        else {
            panic!("expected reconciled cancellation");
        };
        assert_eq!(commands.len(), preview.operations().len());
        assert_eq!(*failure, SystemConfiguratorError::Cancelled);
        assert!(repository.snapshots.borrow().is_empty());
    }

    #[test]
    fn restart_readiness_failures_are_distinct() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let mut snapshot = snapshot_with_configuration("verbose: true\n");
        snapshot.set_status(
            crate::domain::parse_status(
                r#"{"engine":"cpu","services":{"myna-parakeet.service":"failed"}}"#,
            )
            .unwrap(),
        );
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(result, Err(ApplyFailure::RestartReadiness { .. })));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn readiness_requires_positive_service_health() {
        let mut snapshot = snapshot_with_configuration("verbose: true\n");
        snapshot.set_status(crate::domain::parse_status(r#"{"engine":"cpu"}"#).unwrap());

        let failure = readiness_failure(&snapshot);

        assert!(failure
            .as_deref()
            .is_some_and(|message| message.contains("no service health")));
    }

    #[test]
    fn transient_restart_unavailability_is_polled_until_ready() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let mut restarting = snapshot_with_configuration("verbose: true\n");
        restarting.add_error(crate::domain::BackendSurfaceError::new(
            BackendSurface::Status,
            "backend is restarting",
            "socket unavailable",
        ));
        let ready = snapshot_with_configuration("verbose: true\n");
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([restarting, ready]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(result.is_ok());
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn transient_readback_unavailability_is_polled_until_complete() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let mut incomplete = snapshot_with_configuration("verbose: true\n");
        incomplete.add_error(crate::domain::BackendSurfaceError::new(
            BackendSurface::ModelctlConfig,
            "backend configuration is temporarily unavailable",
            "snap change in progress",
        ));
        let complete = snapshot_with_configuration("verbose: true\n");
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([incomplete, complete]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(result.is_ok());
        assert_eq!(configurator.call_count(), 1);
        assert!(repository.snapshots.borrow().is_empty());
    }

    #[test]
    fn snap_config_read_failure_is_not_reported_as_a_value_mismatch() {
        let preview = ApplyPreview::new(
            backend(),
            vec![
                StagedChange::new("ratio", ConfigValue::Number(0.25), ConfigValue::Number(0.5))
                    .unwrap(),
            ],
        )
        .unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let mut readback = snapshot_with_configuration("ratio: 0.25\n");
        readback.add_error(crate::domain::BackendSurfaceError::new(
            BackendSurface::ModelctlConfig,
            "modelctl get failed",
            "read denied",
        ));
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from(vec![
                readback;
                READINESS_ATTEMPTS
            ]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        let ApplyFailure::ReadBackUnavailable { errors, .. } = result.unwrap_err() else {
            panic!("expected unavailable read-back");
        };
        assert_eq!(errors[0].surface(), BackendSurface::ModelctlConfig);
        assert_eq!(errors[0].stderr(), "read denied");
    }

    #[test]
    fn a_value_the_model_did_not_keep_is_a_mismatch() {
        let page = page_with_snapshot(snapshot_with_configuration("sleep-idle-seconds: 30\n"));
        let preview =
            prepare_change(&page, "sleep-idle-seconds", ConfigValue::Integer(60)).unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "sleep-idle-seconds: 30\n",
            )]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        let ApplyFailure::ReadBackMismatch { mismatches, .. } = result.unwrap_err() else {
            panic!("expected mismatch");
        };
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].key(), "sleep-idle-seconds");
        assert_eq!(mismatches[0].actual(), Some(&ConfigValue::Integer(30)));
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn a_change_is_applied_in_one_authorization_and_read_back() {
        let page = page_with_snapshot(snapshot_with_configuration("verbose: false\n"));
        let preview = prepare_change(&page, "verbose", ConfigValue::Boolean(true)).unwrap();
        let configurator = FakeConfigurator::scripted([Ok(successful_results(&preview))]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "verbose: true\n",
            )]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ))
        .unwrap();

        assert_eq!(
            result.snapshot().value("verbose"),
            Some(ConfigValue::Boolean(true))
        );
        assert_eq!(configurator.call_count(), 1);
    }

    #[test]
    fn pkexec_adapter_executes_the_shell_free_plan_in_one_invocation() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let runner = FakeCommandRunner::scripted([ok(&plan_output(preview.operations(), None))]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner.clone()))
            .with_executor("/usr/bin/myna-config");

        let results =
            block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].executable(), "pkexec");
        assert_eq!(calls[0].arguments()[0], "/usr/bin/myna-config");
        assert_eq!(calls[0].arguments()[1], "--apply-plan");
        assert_eq!(
            calls[0].arguments()[2],
            crate::apply_plan::encode_plan(
                crate::apply_plan::PlanKind::Apply,
                preview.operations()
            )
            .unwrap()
        );
        assert_eq!(results, successful_results(&preview));
    }

    #[test]
    fn later_operation_failure_preserves_success_and_reads_back_persisted_state() {
        let page = page_with_snapshot(snapshot_with_configuration("verbose: false\n"));
        let preview = prepare_change(&page, "verbose", ConfigValue::Boolean(true)).unwrap();
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(1),
            stdout: plan_output(preview.operations(), Some((1, "restart failed"))),
            stderr: String::new(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner.clone()));
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "verbose: true\n",
            )]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &adapter,
            &repository,
            CancellationToken::new(),
        ));

        let ApplyFailure::PartialExecution {
            snapshot,
            commands,
            failure,
        } = result.unwrap_err()
        else {
            panic!("expected reconciled partial failure");
        };
        assert!(matches!(
            *failure,
            SystemConfiguratorError::Execution {
                step: FailedStep::Command {
                    exit_status: Some(1),
                    ref arguments,
                    ..
                },
                ..
            } if arguments == &["restart", "myna-parakeet"]
        ));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].arguments(), preview.operations()[0].arguments());
        assert_eq!(
            snapshot.configuration().get("verbose"),
            Some(&ConfigValue::Boolean(true))
        );
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn cancellation_during_the_plan_is_reported_without_read_back() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let runner = FakeCommandRunner::scripted([Err(CommandError::Cancelled)]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner));
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "verbose: true\n",
            )]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &adapter,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(result, Err(ApplyFailure::CancelledExecution)));
        assert_eq!(repository.snapshots.borrow().len(), 1);
    }

    #[test]
    fn required_apply_stops_when_explicit_restart_fails() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(1),
            stdout: plan_output(preview.operations(), Some((1, "restart failed"))),
            stderr: String::new(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner.clone()));

        let error =
            block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap_err();

        assert_eq!(error.completed().len(), 1);
        assert!(matches!(
            error.error(),
            SystemConfiguratorError::Execution {
                step: FailedStep::Command {
                    arguments,
                    stderr,
                    ..
                },
                ..
            } if arguments == &["restart", "myna-parakeet"]
                && stderr == "restart failed"
        ));
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn required_apply_cannot_verify_when_restart_result_is_missing() {
        let preview = ApplyPreview::new(
            backend(),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
        )
        .unwrap();
        let incomplete = vec![successful_results(&preview)[0].clone()];
        let configurator = FakeConfigurator::scripted([Ok(incomplete)]);
        let repository = FakeRepository {
            snapshots: Rc::new(RefCell::new(VecDeque::from([snapshot_with_configuration(
                "verbose: true\n",
            )]))),
        };

        let result = block_on(execute_backend_apply(
            &preview,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));

        assert!(matches!(
            result,
            Err(ApplyFailure::PartialExecution { failure, .. })
                if matches!(*failure, SystemConfiguratorError::Execution { .. })
        ));
        assert!(repository.snapshots.borrow().is_empty());
    }
}
