use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::active_backend::SwitchPlan;
use crate::adapters::snapd_client::{
    is_valid_slot_name, is_valid_snap_name, InterfaceAction, SnapdClient, SnapdError,
    UnixSocketSnapdClient,
};
use crate::apply_plan::{self, APPLY_PLAN_FLAG};
use crate::backend_apply::ApplyPreview;
use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::domain::CommandResult;
use crate::ports::{SystemConfigurator, SystemConfiguratorError, SystemConfiguratorFailure};

const APPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// Fixed plug reference the direct snapd adapter is willing to send. Any
/// switch step whose typed target does not match these exact allowlists is
/// rejected without reaching the socket.
const ALLOWED_PLUG: &str = "myna:backend";
const ALLOWED_RESTART_SERVICE: &str = "myna.myna";

#[derive(Clone, Debug, PartialEq, Eq)]
enum SwitchStep {
    Interface {
        request: CommandRequest,
        action: InterfaceAction,
    },
    Restart {
        request: CommandRequest,
    },
}

/// Adapter that:
///
/// * executes backend switch operations directly against the host snapd REST
///   API over `/run/snapd.socket` (no `pkexec`, no shell), running blocking
///   socket I/O off the GTK main loop, and
/// * runs a backend setting `apply` as one `pkexec` invocation of this same
///   binary in [`crate::apply_plan`] executor mode, so the user authorizes
///   once per apply rather than once per command.
pub struct PkexecSystemConfigurator {
    runner: Arc<dyn CommandRunner>,
    snapd: Arc<dyn SnapdClient>,
    executor: PathBuf,
}

impl PkexecSystemConfigurator {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_snapd_client(runner, Arc::new(UnixSocketSnapdClient::new()))
    }

    /// Construct with a custom snapd client. Used by tests to point the
    /// adapter at a fake Unix socket server.
    pub fn with_snapd_client(runner: Arc<dyn CommandRunner>, snapd: Arc<dyn SnapdClient>) -> Self {
        Self {
            runner,
            snapd,
            executor: default_executor(),
        }
    }

    /// Override the binary `pkexec` runs in executor mode.
    pub fn with_executor(mut self, executor: impl Into<PathBuf>) -> Self {
        self.executor = executor.into();
        self
    }
}

/// The executor is this binary. `pkexec` needs an absolute path and resolves
/// nothing through `$PATH` of the caller.
fn default_executor() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("myna-config"))
}

#[async_trait(?Send)]
impl SystemConfigurator for PkexecSystemConfigurator {
    async fn execute_privileged(
        &self,
        operations: &[CommandRequest],
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        execute_apply_plan(
            self.runner.as_ref(),
            &self.executor,
            operations,
            cancellation,
        )
        .await
    }

    async fn execute_backend_switch(
        &self,
        plan: &SwitchPlan,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        let actions = validate_switch_plan(plan)?;
        let mut completed: Vec<CommandResult> = Vec::with_capacity(actions.len());
        for step in actions {
            if cancellation.is_cancelled() {
                return Err(SystemConfiguratorFailure::new(
                    completed,
                    SystemConfiguratorError::Cancelled,
                ));
            }
            let request = match &step {
                SwitchStep::Interface { request, .. } | SwitchStep::Restart { request } => {
                    request.clone()
                }
            };
            let outcome = match step {
                SwitchStep::Interface { action, .. } => self
                    .snapd
                    .apply_interface_action(action, cancellation.clone())
                    .await
                    .map(|_| ()),
                SwitchStep::Restart { .. } => self
                    .snapd
                    .restart_myna_service(cancellation.clone())
                    .await
                    .map(|_| ()),
            };
            match outcome {
                Ok(_) => {
                    completed.push(CommandResult::new(
                        request.executable().to_owned(),
                        request.arguments().to_vec(),
                        Some(0),
                        String::new(),
                        String::new(),
                    ));
                }
                Err(error) => {
                    return Err(SystemConfiguratorFailure::new(
                        completed,
                        snapd_error_to_system_error(request, error),
                    ));
                }
            }
        }
        Ok(completed)
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

/// Ensure every operation matches the exact allowlist for the direct snapd
/// adapter: ordered `snap disconnect` calls, exactly one `snap connect`, and a
/// final exact `snap restart myna.myna`. Anything else is refused before we
/// open a socket.
#[allow(clippy::result_large_err)]
fn validate_switch_plan(plan: &SwitchPlan) -> Result<Vec<SwitchStep>, SystemConfiguratorFailure> {
    if plan.operations().is_empty() {
        return Ok(Vec::new());
    }

    let mut result = Vec::with_capacity(plan.operations().len());
    for request in plan.operations() {
        let step = validate_operation(request).map_err(|message| {
            SystemConfiguratorFailure::new(
                Vec::new(),
                SystemConfiguratorError::execution(
                    "snapd",
                    request.arguments().to_vec(),
                    None,
                    String::new(),
                    message,
                ),
            )
        })?;
        result.push(step);
    }

    let connect_indices = result
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            SwitchStep::Interface {
                action: InterfaceAction::Connect { .. },
                ..
            } => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();
    let restart_indices = result
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            SwitchStep::Restart { .. } => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();

    if connect_indices.is_empty() {
        return Err(invalid_switch_plan(
            plan.operations()
                .last()
                .expect("non-noop switch plan has operations"),
            "switch plan is missing the final connect",
        ));
    }
    if connect_indices.len() != 1 {
        return Err(invalid_switch_plan(
            request_for_step(&result[connect_indices[1]]),
            "switch plan must contain exactly one connect",
        ));
    }
    if restart_indices.is_empty() {
        return Err(invalid_switch_plan(
            plan.operations()
                .last()
                .expect("non-noop switch plan has operations"),
            "switch plan is missing the final restart",
        ));
    }
    if restart_indices.len() != 1 {
        return Err(invalid_switch_plan(
            request_for_step(&result[restart_indices[1]]),
            "duplicate restart in switch plan",
        ));
    }

    let connect_index = connect_indices[0];
    let restart_index = restart_indices[0];
    if restart_index <= connect_index {
        return Err(invalid_switch_plan(
            request_for_step(&result[restart_index]),
            "restart must follow the final connect",
        ));
    }
    if restart_index != result.len() - 1 {
        return Err(invalid_switch_plan(
            request_for_step(&result[restart_index]),
            "restart must be the final switch operation",
        ));
    }
    if let Some((_, step)) = result
        .iter()
        .enumerate()
        .skip(connect_index + 1)
        .take(restart_index - connect_index - 1)
        .find(|(_, step)| {
            matches!(
                step,
                SwitchStep::Interface {
                    action: InterfaceAction::Disconnect { .. },
                    ..
                }
            )
        })
    {
        return Err(invalid_switch_plan(
            request_for_step(step),
            "disconnect cannot follow connect",
        ));
    }
    Ok(result)
}

fn validate_operation(request: &CommandRequest) -> Result<SwitchStep, String> {
    if request.executable() != "snap" {
        return Err(format!(
            "unexpected executable in switch plan: {}",
            request.executable()
        ));
    }
    let args = request.arguments();
    match args.first().map(String::as_str) {
        Some("restart") => {
            if args != ["restart", ALLOWED_RESTART_SERVICE] {
                return Err("restart must be exactly `snap restart myna.myna`".to_owned());
            }
            Ok(SwitchStep::Restart {
                request: request.clone(),
            })
        }
        Some("connect" | "disconnect") => {
            if args.len() != 3 {
                return Err(format!(
                    "switch plan operation must have 3 arguments, got {}",
                    args.len()
                ));
            }
            if args[1] != ALLOWED_PLUG {
                return Err(format!("unexpected plug {}", args[1]));
            }
            let (backend, slot) = args[2]
                .split_once(':')
                .ok_or_else(|| format!("malformed slot {}", args[2]))?;
            if !is_valid_snap_name(backend) {
                return Err(format!("invalid backend snap name: {backend}"));
            }
            if !is_valid_slot_name(slot) {
                return Err(format!("invalid backend slot name: {slot}"));
            }
            let action = match args[0].as_str() {
                "connect" => InterfaceAction::Connect {
                    backend_snap: backend.to_owned(),
                    backend_slot: slot.to_owned(),
                },
                "disconnect" => InterfaceAction::Disconnect {
                    backend_snap: backend.to_owned(),
                    backend_slot: slot.to_owned(),
                },
                _ => unreachable!(),
            };
            Ok(SwitchStep::Interface {
                request: request.clone(),
                action,
            })
        }
        Some(other) => Err(format!("unknown snap action {other}")),
        None => Err("switch plan operation has no arguments".to_owned()),
    }
}

fn invalid_switch_plan(request: &CommandRequest, message: &str) -> SystemConfiguratorFailure {
    SystemConfiguratorFailure::new(
        Vec::new(),
        SystemConfiguratorError::execution(
            "snapd",
            request.arguments().to_vec(),
            None,
            String::new(),
            message,
        ),
    )
}

fn request_for_step(step: &SwitchStep) -> &CommandRequest {
    match step {
        SwitchStep::Interface { request, .. } | SwitchStep::Restart { request } => request,
    }
}

fn snapd_timeout_message(
    elapsed: Duration,
    context: crate::adapters::snapd_client::SnapdTimeoutContext,
) -> String {
    format!("{} timed out after {elapsed:?}", context.description())
}

pub(crate) fn snapd_error_to_system_error(
    request: CommandRequest,
    error: SnapdError,
) -> SystemConfiguratorError {
    let arguments = request.arguments().to_vec();
    match error {
        SnapdError::Cancelled => SystemConfiguratorError::Cancelled,
        SnapdError::AuthorizationDenied {
            status_code,
            message,
            ..
        } => SystemConfiguratorError::authorization_denied(
            "snapd",
            arguments,
            Some(status_code as i32),
            message,
        ),
        SnapdError::Timeout { elapsed, context } => SystemConfiguratorError::execution(
            "snapd",
            arguments,
            None,
            String::new(),
            snapd_timeout_message(elapsed, context),
        ),
        SnapdError::ResponseTooLarge => SystemConfiguratorError::execution(
            "snapd",
            arguments,
            None,
            String::new(),
            "snapd response exceeded the client size limit",
        ),
        SnapdError::Transport { message } => SystemConfiguratorError::execution(
            "snapd",
            arguments,
            None,
            String::new(),
            format!("snapd transport error: {message}"),
        ),
        SnapdError::Protocol { message, .. } => SystemConfiguratorError::execution(
            "snapd",
            arguments,
            None,
            String::new(),
            format!("snapd protocol error: {message}"),
        ),
        SnapdError::Snapd {
            status_code,
            message,
            ..
        } => SystemConfiguratorError::execution(
            "snapd",
            arguments,
            Some(status_code as i32),
            String::new(),
            message,
        ),
    }
}

/// Run the whole plan as root under a single `pkexec`. The executor prints
/// one result per operation it ran, stopping at the first failure, so a
/// partial apply still reports exactly which commands completed.
#[allow(clippy::result_large_err)]
async fn execute_apply_plan(
    runner: &dyn CommandRunner,
    executor: &Path,
    operations: &[CommandRequest],
    cancellation: CancellationToken,
) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
    let plan = apply_plan::encode_plan(operations).map_err(|message| {
        SystemConfiguratorFailure::new(
            Vec::new(),
            SystemConfiguratorError::execution(
                "pkexec",
                Vec::new(),
                None,
                String::new(),
                format!("invalid apply plan: {message}"),
            ),
        )
    })?;
    let arguments = vec![
        executor.to_string_lossy().into_owned(),
        APPLY_PLAN_FLAG.to_owned(),
        plan,
    ];
    let request =
        CommandRequest::new("pkexec".to_owned(), arguments.clone()).with_timeout(APPLY_TIMEOUT);
    match runner.run(request, cancellation).await {
        Ok(output) => match apply_plan::decode_results(output.stdout()) {
            Ok(results) => Ok(results),
            Err(message) => Err(SystemConfiguratorFailure::new(
                Vec::new(),
                SystemConfiguratorError::execution(
                    "pkexec",
                    arguments,
                    output.exit_status(),
                    output.stderr(),
                    format!("apply plan produced unreadable results: {message}"),
                ),
            )),
        },
        Err(CommandError::NonZero {
            exit_status,
            stdout,
            stderr,
        }) => {
            if matches!(exit_status, Some(126 | 127)) {
                return Err(SystemConfiguratorFailure::new(
                    Vec::new(),
                    SystemConfiguratorError::authorization_denied(
                        "pkexec",
                        arguments,
                        exit_status,
                        stderr,
                    ),
                ));
            }
            match apply_plan::decode_results(&stdout) {
                Ok(mut results) => match results.pop() {
                    Some(failed) if failed.exit_status() != Some(0) => Err(
                        SystemConfiguratorFailure::new(results, map_operation_failure(&failed)),
                    ),
                    _ => Err(SystemConfiguratorFailure::new(
                        results,
                        SystemConfiguratorError::execution(
                            "pkexec",
                            arguments,
                            exit_status,
                            stderr,
                            "apply plan failed without reporting a failed operation",
                        ),
                    )),
                },
                Err(_) => Err(SystemConfiguratorFailure::new(
                    Vec::new(),
                    SystemConfiguratorError::execution(
                        "pkexec",
                        arguments,
                        exit_status,
                        stderr.clone(),
                        primary_detail(&stderr, &stdout),
                    ),
                )),
            }
        }
        Err(error) => Err(SystemConfiguratorFailure::new(
            Vec::new(),
            map_command_error("pkexec", &arguments, error),
        )),
    }
}

/// Classify one failed operation out of the plan. A `modelctl set` that exits
/// non-zero has rejected the values (unknown key, bad value); anything else is
/// an execution failure.
fn map_operation_failure(failed: &CommandResult) -> SystemConfiguratorError {
    let arguments = failed.arguments();
    let detail = primary_detail(failed.stderr(), failed.stdout());
    let is_modelctl_set = arguments.first().map(String::as_str) == Some("run")
        && arguments.get(2).map(String::as_str) == Some("set");
    if is_modelctl_set {
        let mut error = SystemConfiguratorError::values_rejected(
            failed.executable(),
            arguments.to_vec(),
            failed.exit_status(),
            failed.stderr(),
        );
        if let SystemConfiguratorError::ValuesRejected { message, .. } = &mut error {
            *message = detail;
        }
        error
    } else {
        SystemConfiguratorError::execution(
            failed.executable(),
            arguments.to_vec(),
            failed.exit_status(),
            failed.stderr(),
            detail,
        )
    }
}

fn map_command_error(
    executable: &str,
    arguments: &[String],
    error: CommandError,
) -> SystemConfiguratorError {
    match error {
        CommandError::Cancelled => SystemConfiguratorError::Cancelled,
        CommandError::NonZero {
            exit_status,
            stdout,
            stderr,
        } => {
            let detail = primary_detail(&stderr, &stdout);
            if executable == "pkexec" && matches!(exit_status, Some(126 | 127)) {
                SystemConfiguratorError::authorization_denied(
                    executable,
                    arguments.to_vec(),
                    exit_status,
                    stderr,
                )
            } else {
                SystemConfiguratorError::execution(
                    executable,
                    arguments.to_vec(),
                    exit_status,
                    stderr,
                    detail,
                )
            }
        }
        CommandError::Timeout { timeout } => SystemConfiguratorError::execution(
            executable,
            arguments.to_vec(),
            None,
            String::new(),
            format!("timed out after {timeout:?}"),
        ),
        CommandError::NotFound {
            executable: missing,
        } => SystemConfiguratorError::execution(
            executable,
            arguments.to_vec(),
            None,
            String::new(),
            format!("command not found: {missing}"),
        ),
        CommandError::Spawn { message, .. } => SystemConfiguratorError::execution(
            executable,
            arguments.to_vec(),
            None,
            String::new(),
            message,
        ),
        CommandError::InvalidUtf8 { message, .. } => SystemConfiguratorError::execution(
            executable,
            arguments.to_vec(),
            None,
            String::new(),
            message,
        ),
        CommandError::FakeScriptExhausted => SystemConfiguratorError::execution(
            executable,
            arguments.to_vec(),
            None,
            String::new(),
            "fake command runner has no scripted outcome",
        ),
    }
}

fn primary_detail(stderr: &str, stdout: &str) -> String {
    if !stderr.trim().is_empty() {
        stderr.trim().to_owned()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        "privileged command failed".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gio::glib::MainContext;

    use super::*;
    use crate::apply_plan::plan_output;
    use crate::command::{CommandOutput, FakeCommandRunner};
    use crate::domain::{
        parse_connections, BackendIdentity, ConfigScope, ConfigValue, StagedChange,
    };

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        MainContext::new().block_on(future)
    }

    fn preview() -> ApplyPreview {
        ApplyPreview::new(
            BackendIdentity::new("myna-parakeet", "provider")
                .with_modelctl_app("myna-parakeet.parakeet"),
            vec![StagedChange::new(
                ConfigScope::Package,
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
                true,
            )
            .unwrap()],
        )
        .unwrap()
    }

    #[test]
    fn localized_pkexec_exit_126_and_127_map_to_authorization_denied() {
        for exit_status in [126, 127] {
            let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
                exit_status: Some(exit_status),
                stdout: String::new(),
                stderr: "Autorisierung abgelehnt".to_owned(),
            })]);
            let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

            let error =
                block_on(adapter.apply_backend_config(&preview(), CancellationToken::new()))
                    .unwrap_err();

            assert!(matches!(
                error.error(),
                SystemConfiguratorError::AuthorizationDenied {
                    exit_status: Some(actual),
                    stderr,
                    ..
                } if *actual == exit_status && stderr == "Autorisierung abgelehnt"
            ));
        }
    }

    #[test]
    fn empty_pkexec_exit_126_and_127_map_to_authorization_denied() {
        for exit_status in [126, 127] {
            let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
                exit_status: Some(exit_status),
                stdout: String::new(),
                stderr: String::new(),
            })]);
            let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

            let error =
                block_on(adapter.apply_backend_config(&preview(), CancellationToken::new()))
                    .unwrap_err();

            assert!(matches!(
                error.error(),
                SystemConfiguratorError::AuthorizationDenied {
                    exit_status: Some(actual),
                    stderr,
                    ..
                } if *actual == exit_status && stderr.is_empty()
            ));
        }
    }

    #[test]
    fn the_whole_plan_runs_through_one_pkexec_invocation_of_the_executor() {
        let preview = preview();
        let runner = FakeCommandRunner::scripted([Ok(CommandOutput::new(
            Some(0),
            plan_output(preview.operations(), None),
            "",
        ))]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner.clone()))
            .with_executor("/opt/myna/bin/myna-config");

        let results =
            block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].executable(), "pkexec");
        assert_eq!(
            &calls[0].arguments()[..2],
            ["/opt/myna/bin/myna-config", APPLY_PLAN_FLAG]
        );
        assert_eq!(
            calls[0].arguments()[2],
            apply_plan::encode_plan(preview.operations()).unwrap()
        );
        assert_eq!(results.len(), preview.operations().len());
        for (result, operation) in results.iter().zip(preview.operations()) {
            assert_eq!(result.executable(), operation.executable());
            assert_eq!(result.arguments(), operation.arguments());
            assert_eq!(result.exit_status(), Some(0));
        }
    }

    #[test]
    fn a_failed_modelctl_set_is_a_values_rejection_with_the_operation_argv() {
        let preview = preview();
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(1),
            stdout: plan_output(preview.operations(), Some((0, "unknown key verbose"))),
            stderr: String::new(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

        let error =
            block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap_err();

        assert!(error.completed().is_empty());
        assert!(matches!(
            error.error(),
            SystemConfiguratorError::ValuesRejected {
                executable,
                arguments,
                exit_status: Some(1),
                stderr,
                message,
            } if executable == "snap"
                && arguments == preview.operations()[0].arguments()
                && stderr == "unknown key verbose"
                && message == "unknown key verbose"
        ));
    }

    #[test]
    fn a_failed_later_operation_keeps_the_completed_prefix() {
        let preview = preview();
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(1),
            stdout: plan_output(preview.operations(), Some((1, "restart failed"))),
            stderr: String::new(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

        let error =
            block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap_err();

        assert_eq!(error.completed().len(), 1);
        assert_eq!(
            error.completed()[0].arguments(),
            preview.operations()[0].arguments()
        );
        assert!(matches!(
            error.error(),
            SystemConfiguratorError::Execution {
                arguments,
                stderr,
                ..
            } if arguments == &["restart", "myna-parakeet"] && stderr == "restart failed"
        ));
    }

    #[test]
    fn unreadable_executor_output_is_an_execution_failure_carrying_stderr() {
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(2),
            stdout: String::new(),
            stderr: "myna-config: invalid apply plan: unexpected executable sh".to_owned(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

        let error = block_on(adapter.apply_backend_config(&preview(), CancellationToken::new()))
            .unwrap_err();

        assert!(error.completed().is_empty());
        assert!(matches!(
            error.error(),
            SystemConfiguratorError::Execution {
                executable,
                exit_status: Some(2),
                message,
                ..
            } if executable == "pkexec" && message.contains("unexpected executable sh")
        ));
    }

    #[test]
    fn non_pkexec_permission_error_is_an_execution_failure() {
        let error = map_command_error(
            "snap",
            &["restart".to_owned(), "myna-parakeet".to_owned()],
            CommandError::NonZero {
                exit_status: Some(126),
                stdout: String::new(),
                stderr: "permission denied".to_owned(),
            },
        );

        assert!(matches!(
            error,
            SystemConfiguratorError::Execution {
                exit_status: Some(126),
                ref stderr,
                ..
            } if stderr == "permission denied"
        ));
    }

    #[test]
    fn modelctl_permission_error_is_an_execution_failure() {
        let error = map_command_error(
            "pkexec",
            &[
                "snap".to_owned(),
                "run".to_owned(),
                "myna-parakeet.modelctl".to_owned(),
                "use-model".to_owned(),
                "new".to_owned(),
            ],
            CommandError::NonZero {
                exit_status: Some(1),
                stdout: String::new(),
                stderr: "permission denied".to_owned(),
            },
        );

        assert!(matches!(
            error,
            SystemConfiguratorError::Execution {
                exit_status: Some(1),
                ref stderr,
                ..
            } if stderr == "permission denied"
        ));
    }

    #[derive(Default)]
    struct ScriptedSnapd {
        interface_outcomes:
            Mutex<Vec<Result<crate::adapters::snapd_client::SnapdOutcome, SnapdError>>>,
        restart_outcomes:
            Mutex<Vec<Result<crate::adapters::snapd_client::ChangeReport, SnapdError>>>,
        calls: Mutex<Vec<SnapdCall>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SnapdCall {
        Interface(InterfaceAction),
        RestartMynaService,
    }

    #[async_trait(?Send)]
    impl SnapdClient for ScriptedSnapd {
        async fn apply_interface_action(
            &self,
            action: InterfaceAction,
            cancellation: CancellationToken,
        ) -> Result<crate::adapters::snapd_client::SnapdOutcome, SnapdError> {
            self.calls
                .lock()
                .unwrap()
                .push(SnapdCall::Interface(action));
            if cancellation.is_cancelled() {
                return Err(SnapdError::Cancelled);
            }
            self.interface_outcomes
                .lock()
                .unwrap()
                .drain(..1)
                .next()
                .unwrap_or_else(|| {
                    Err(SnapdError::Protocol {
                        message: "no scripted outcome".into(),
                        body: String::new(),
                    })
                })
        }

        async fn restart_myna_service(
            &self,
            cancellation: CancellationToken,
        ) -> Result<crate::adapters::snapd_client::ChangeReport, SnapdError> {
            self.calls
                .lock()
                .unwrap()
                .push(SnapdCall::RestartMynaService);
            if cancellation.is_cancelled() {
                return Err(SnapdError::Cancelled);
            }
            self.restart_outcomes
                .lock()
                .unwrap()
                .drain(..1)
                .next()
                .unwrap_or_else(|| {
                    Err(SnapdError::Protocol {
                        message: "no scripted restart outcome".into(),
                        body: String::new(),
                    })
                })
        }
    }

    fn switch_plan() -> SwitchPlan {
        let snapshot = parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend old:provider manual\n\
             content - new:provider -\n",
            "name: content\nslots:\n  - old:provider:\n      content: inference-provider\n  - new:provider:\n      content: inference-provider\n",
        )
        .unwrap();
        SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap()
    }

    fn invalid_switch_plan(operations: Vec<CommandRequest>) -> SwitchPlan {
        let snapshot = parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend old:provider manual\n\
             content - new:provider -\n",
            "name: content\nslots:\n  - old:provider:\n      content: inference-provider\n  - new:provider:\n      content: inference-provider\n",
        )
        .unwrap();
        SwitchPlan::with_operations_for_test(
            snapshot,
            BackendIdentity::new("new", "provider"),
            operations,
        )
    }

    #[test]
    fn backend_switch_runs_disconnect_then_connect_then_restart_via_snapd_client() {
        use crate::adapters::snapd_client::{ChangeReport, SnapdOutcome};
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Ok(SnapdOutcome::Sync), Ok(SnapdOutcome::Sync)]),
            restart_outcomes: Mutex::new(vec![Ok(ChangeReport {
                change_id: "7".into(),
                status: "Done".into(),
            })]),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::default());
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner, snapd.clone());

        let completed =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap();
        assert_eq!(completed.len(), 3);
        let calls = snapd.calls.lock().unwrap().clone();
        assert!(
            matches!(&calls[0], SnapdCall::Interface(InterfaceAction::Disconnect { backend_snap, backend_slot }) if backend_snap == "old" && backend_slot == "provider")
        );
        assert!(
            matches!(&calls[1], SnapdCall::Interface(InterfaceAction::Connect { backend_snap, backend_slot }) if backend_snap == "new" && backend_slot == "provider")
        );
        assert_eq!(calls[2], SnapdCall::RestartMynaService);
    }

    #[test]
    fn backend_switch_partial_failure_preserves_completed_disconnect() {
        use crate::adapters::snapd_client::SnapdOutcome;
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![
                Ok(SnapdOutcome::Sync),
                Err(SnapdError::Snapd {
                    status_code: 400,
                    kind: None,
                    message: "connect failed".into(),
                }),
            ]),
            restart_outcomes: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::default());
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner, snapd);

        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap_err();
        assert_eq!(failure.completed().len(), 1);
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. } if message == "connect failed"
        ));
    }

    #[test]
    fn backend_switch_maps_snapd_authorization_denial() {
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Err(SnapdError::AuthorizationDenied {
                status_code: 403,
                kind: Some("auth-cancelled".into()),
                message: "cancelled".into(),
            })]),
            restart_outcomes: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::default());
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner, snapd);

        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap_err();
        assert!(failure.completed().is_empty());
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::AuthorizationDenied { .. }
        ));
    }

    #[test]
    fn backend_switch_cancellation_stops_after_first_operation() {
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(Vec::new()),
            restart_outcomes: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::default());
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner, snapd.clone());
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();

        // Cancel synchronously between the first and second operations by having
        // ScriptedSnapd's second call see an already-cancelled token. We drive
        // that by pre-cancelling after the first script slot is consumed via
        // an ordering-agnostic assertion: cancel immediately, then execute.
        cancel.cancel();
        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), cancellation)).unwrap_err();
        assert_eq!(failure.error(), &SystemConfiguratorError::Cancelled);
        assert!(snapd.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn backend_switch_restart_failure_preserves_completed_interface_operations() {
        use crate::adapters::snapd_client::SnapdOutcome;
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Ok(SnapdOutcome::Sync), Ok(SnapdOutcome::Sync)]),
            restart_outcomes: Mutex::new(vec![Err(SnapdError::Snapd {
                status_code: 500,
                kind: None,
                message: "restart failed".into(),
            })]),
            calls: Mutex::new(Vec::new()),
        });
        let adapter = PkexecSystemConfigurator::with_snapd_client(
            Arc::new(FakeCommandRunner::default()),
            snapd.clone(),
        );

        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap_err();
        assert_eq!(failure.completed().len(), 2);
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. } if message == "restart failed"
        ));
        let calls = snapd.calls.lock().unwrap().clone();
        assert_eq!(calls.last(), Some(&SnapdCall::RestartMynaService));
    }

    #[test]
    fn backend_switch_rejects_missing_restart_for_mutation_plan() {
        let plan = invalid_switch_plan(vec![
            CommandRequest::new(
                "snap".into(),
                vec![
                    "disconnect".into(),
                    "myna:backend".into(),
                    "old:provider".into(),
                ],
            ),
            CommandRequest::new(
                "snap".into(),
                vec![
                    "connect".into(),
                    "myna:backend".into(),
                    "new:provider".into(),
                ],
            ),
        ]);
        let snapd = Arc::new(ScriptedSnapd::default());
        let adapter = PkexecSystemConfigurator::with_snapd_client(
            Arc::new(FakeCommandRunner::default()),
            snapd.clone(),
        );

        let failure =
            block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap_err();
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. }
                if message == "switch plan is missing the final restart"
        ));
        assert!(snapd.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn backend_switch_rejects_restart_before_connect_duplicate_restart_and_arbitrary_service() {
        let plans = [
            (
                invalid_switch_plan(vec![
                    CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
                    CommandRequest::new(
                        "snap".into(),
                        vec![
                            "connect".into(),
                            "myna:backend".into(),
                            "new:provider".into(),
                        ],
                    ),
                ]),
                "restart must follow the final connect",
            ),
            (
                invalid_switch_plan(vec![
                    CommandRequest::new(
                        "snap".into(),
                        vec![
                            "connect".into(),
                            "myna:backend".into(),
                            "new:provider".into(),
                        ],
                    ),
                    CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
                    CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
                ]),
                "duplicate restart in switch plan",
            ),
            (
                invalid_switch_plan(vec![
                    CommandRequest::new(
                        "snap".into(),
                        vec![
                            "connect".into(),
                            "myna:backend".into(),
                            "new:provider".into(),
                        ],
                    ),
                    CommandRequest::new(
                        "snap".into(),
                        vec!["restart".into(), "other.service".into()],
                    ),
                ]),
                "restart must be exactly `snap restart myna.myna`",
            ),
        ];

        for (plan, expected) in plans {
            let snapd = Arc::new(ScriptedSnapd::default());
            let adapter = PkexecSystemConfigurator::with_snapd_client(
                Arc::new(FakeCommandRunner::default()),
                snapd.clone(),
            );
            let failure = block_on(adapter.execute_backend_switch(&plan, CancellationToken::new()))
                .unwrap_err();
            assert!(matches!(
                failure.error(),
                SystemConfiguratorError::Execution { message, .. } if message == expected
            ));
            assert!(snapd.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn switch_operation_carries_any_valid_slot_name_to_snapd() {
        let request = CommandRequest::new(
            "snap".into(),
            vec![
                "disconnect".into(),
                "myna:backend".into(),
                "community-asr:speech".into(),
            ],
        );
        assert_eq!(
            validate_operation(&request),
            Ok(SwitchStep::Interface {
                request: request.clone(),
                action: InterfaceAction::Disconnect {
                    backend_snap: "community-asr".into(),
                    backend_slot: "speech".into(),
                },
            })
        );
    }

    #[test]
    fn switch_operation_rejects_a_malformed_slot() {
        for (slot, expected) in [
            ("new", "malformed slot new"),
            ("new:Provider", "invalid backend slot name: Provider"),
            ("new:", "invalid backend slot name: "),
            (
                "new:provider:extra",
                "invalid backend slot name: provider:extra",
            ),
            ("bad;snap:provider", "invalid backend snap name: bad;snap"),
        ] {
            let request = CommandRequest::new(
                "snap".into(),
                vec!["connect".into(), "myna:backend".into(), slot.into()],
            );
            assert_eq!(validate_operation(&request), Err(expected.to_owned()));
        }
    }

    #[test]
    fn backend_switch_rejects_duplicate_restart_when_final() {
        let plan = invalid_switch_plan(vec![
            CommandRequest::new(
                "snap".into(),
                vec![
                    "connect".into(),
                    "myna:backend".into(),
                    "new:provider".into(),
                ],
            ),
            CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
            CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
        ]);
        let adapter = PkexecSystemConfigurator::with_snapd_client(
            Arc::new(FakeCommandRunner::default()),
            Arc::new(ScriptedSnapd::default()),
        );

        let failure =
            block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap_err();
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. }
                if message == "duplicate restart in switch plan"
        ));
    }

    #[test]
    fn backend_switch_rejects_disconnect_after_connect() {
        let plan = invalid_switch_plan(vec![
            CommandRequest::new(
                "snap".into(),
                vec![
                    "connect".into(),
                    "myna:backend".into(),
                    "new:provider".into(),
                ],
            ),
            CommandRequest::new(
                "snap".into(),
                vec![
                    "disconnect".into(),
                    "myna:backend".into(),
                    "old:provider".into(),
                ],
            ),
            CommandRequest::new("snap".into(), vec!["restart".into(), "myna.myna".into()]),
        ]);
        let adapter = PkexecSystemConfigurator::with_snapd_client(
            Arc::new(FakeCommandRunner::default()),
            Arc::new(ScriptedSnapd::default()),
        );

        let failure =
            block_on(adapter.execute_backend_switch(&plan, CancellationToken::new())).unwrap_err();
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. }
                if message == "disconnect cannot follow connect"
        ));
    }
}
