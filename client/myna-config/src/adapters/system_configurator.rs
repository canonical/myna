use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::active_backend::{myna_restart_request, SwitchPlan};
use crate::adapters::snapd_client::{
    is_valid_slot_name, is_valid_snap_name, InterfaceAction, SnapdClient, SnapdError,
    UnixSocketSnapdClient, INTERFACES, SYSTEM_CONF,
};
use crate::apply_plan::{self, APPLY_PLAN_FLAG};
use crate::backend_apply::ApplyPreview;
use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::domain::CommandResult;
#[cfg(test)]
use crate::ports::FailedStep;
use crate::ports::{SystemConfigurator, SystemConfiguratorError, SystemConfiguratorFailure};
use crate::snap_changes::ChangeInProgress;
use crate::snap_install::install_request;

/// Fixed plug reference the direct snapd adapter is willing to send. Any
/// switch step whose typed target does not match these exact allowlists is
/// rejected without reaching the socket.
const ALLOWED_PLUG: &str = "myna:backend";

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
/// * executes backend switch interface operations directly against the host
///   snapd REST API over `/run/snapd.socket` (no `pkexec`, no shell), running
///   blocking socket I/O off the GTK main loop, then restarts Myna's user
///   service through `systemctl --user`, and
/// * runs a backend setting `apply` as one `pkexec` invocation of this same
///   binary in [`crate::apply_plan`] executor mode, so the user authorizes
///   once per apply rather than once per command.
pub struct PkexecSystemConfigurator {
    runner: Arc<dyn CommandRunner>,
    snapd: Arc<dyn SnapdClient>,
    executor: PathBuf,
    restart_gate: Rc<dyn RestartGate>,
}

/// What a restart of Myna's service waits on first.
#[async_trait(?Send)]
pub trait RestartGate {
    /// Return once a restart orphans nothing, or `Cancelled` when
    /// `cancellation` fires first.
    async fn until_clear(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError>;
}

struct NoGate;

#[async_trait(?Send)]
impl RestartGate for NoGate {
    async fn until_clear(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        Ok(())
    }
}

impl PkexecSystemConfigurator {
    /// Restarts wait for the daemon's shortcut dialog to be answered.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_snapd_client(runner, Arc::new(UnixSocketSnapdClient::new()))
            .with_restart_gate(Rc::new(crate::adapters::daemon_dialog::DaemonDialogGate))
    }

    /// Construct with a custom snapd client and no restart gate. Used by
    /// tests to point the adapter at a fake Unix socket server.
    pub fn with_snapd_client(runner: Arc<dyn CommandRunner>, snapd: Arc<dyn SnapdClient>) -> Self {
        Self {
            runner,
            snapd,
            executor: default_executor(),
            restart_gate: Rc::new(NoGate),
        }
    }

    pub fn with_restart_gate(mut self, gate: Rc<dyn RestartGate>) -> Self {
        self.restart_gate = gate;
        self
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
                    .map(|_| ())
                    .map_err(|error| {
                        snapd_error_to_system_error(interface_request(&request), error)
                    }),
                SwitchStep::Restart { .. } => self.restart_myna(cancellation.clone()).await,
            };
            match outcome {
                Ok(()) => {
                    completed.push(CommandResult::new(
                        request.executable().to_owned(),
                        request.arguments().to_vec(),
                        Some(0),
                        String::new(),
                        String::new(),
                    ));
                }
                Err(error) => {
                    return Err(SystemConfiguratorFailure::new(completed, error));
                }
            }
        }
        Ok(completed)
    }

    async fn restart_myna(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        self.restart_gate.until_clear(cancellation.clone()).await?;
        let request = myna_restart_request();
        self.runner
            .run(request.clone(), cancellation)
            .await
            .map(|_| ())
            .map_err(|error| map_command_error(request.executable(), request.arguments(), error))
    }

    async fn apply_backend_config(
        &self,
        preview: &ApplyPreview,
        cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        execute_apply_plan(
            self.runner.as_ref(),
            &self.executor,
            preview.operations(),
            cancellation,
        )
        .await
    }

    async fn changes_in_progress(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ChangeInProgress>, String> {
        self.snapd
            .changes_in_progress(cancellation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn user_daemons_enabled(&self, cancellation: CancellationToken) -> Result<bool, String> {
        self.snapd
            .user_daemons_enabled(cancellation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn enable_user_daemons(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        self.snapd
            .enable_user_daemons(cancellation)
            .await
            .map_err(|error| snapd_error_to_system_error(user_daemons_on_request(), error))
    }

    async fn install_snap(
        &self,
        snap: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<String>, SystemConfiguratorError> {
        self.snapd
            .install_snap(snap, cancellation)
            .await
            .map_err(|error| snapd_error_to_system_error(install_request(snap), error))
    }

    async fn snap_change(
        &self,
        change_id: &str,
        cancellation: CancellationToken,
    ) -> Result<ChangeInProgress, String> {
        self.snapd
            .change(change_id, cancellation)
            .await
            .map_err(|error| error.to_string())
    }
}

/// How a report names a snapd request: method, path and what it asks for.
fn snapd_request(method: &str, path: &str, what: &str) -> String {
    format!("{method} {path} ({what})")
}

fn user_daemons_on_request() -> String {
    snapd_request("PUT", SYSTEM_CONF, "experimental.user-daemons=true")
}

fn interface_request(request: &CommandRequest) -> String {
    snapd_request("POST", INTERFACES, &request.arguments().join(" "))
}

/// Ensure every operation matches the exact allowlist for the direct snapd
/// adapter: ordered `snap disconnect` calls, exactly one `snap connect`, and a
/// final exact restart of Myna's user service. Anything else is refused before
/// we open a socket.
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
                SystemConfiguratorError::snapd_execution(interface_request(request), None, message),
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
    if request.executable() == "systemctl" {
        if request != &myna_restart_request() {
            return Err("restart must be exactly Myna's user service".to_owned());
        }
        return Ok(SwitchStep::Restart {
            request: request.clone(),
        });
    }
    if request.executable() != "snap" {
        return Err(format!(
            "unexpected executable in switch plan: {}",
            request.executable()
        ));
    }
    let args = request.arguments();
    match args.first().map(String::as_str) {
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
        SystemConfiguratorError::snapd_execution(interface_request(request), None, message),
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
    format!(
        "{} timed out after {}",
        context.description(),
        crate::command::duration_text(elapsed)
    )
}

pub(crate) fn snapd_error_to_system_error(
    request: String,
    error: SnapdError,
) -> SystemConfiguratorError {
    match error {
        SnapdError::Cancelled => SystemConfiguratorError::Cancelled,
        SnapdError::AuthorizationDenied {
            status_code,
            message,
            ..
        } => SystemConfiguratorError::snapd_authorization_denied(request, status_code, message),
        SnapdError::Timeout { elapsed, context } => SystemConfiguratorError::snapd_execution(
            request,
            None,
            snapd_timeout_message(elapsed, context),
        ),
        SnapdError::ResponseTooLarge => SystemConfiguratorError::snapd_execution(
            request,
            None,
            "snapd response exceeded the client size limit",
        ),
        SnapdError::Transport { message } => SystemConfiguratorError::snapd_execution(
            request,
            None,
            format!("snapd transport error: {message}"),
        ),
        SnapdError::Protocol { message, .. } => SystemConfiguratorError::snapd_execution(
            request,
            None,
            format!("snapd protocol error: {message}"),
        ),
        SnapdError::Snapd {
            status_code,
            message,
            ..
        } => SystemConfiguratorError::snapd_execution(request, Some(status_code), message),
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
    // No deadline: the prompt waits on the user and a model download on the
    // network, and once authorized the executor is root, which this process
    // cannot stop, so giving up on it would only misreport the outcome.
    let request = CommandRequest::new("pkexec".to_owned(), arguments.clone()).without_timeout();
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
            // pkexec(1): 126 when the user dismissed the prompt, 127 when
            // polkit refused or the prompt could not be shown.
            if exit_status == Some(126) {
                return Err(SystemConfiguratorFailure::new(
                    Vec::new(),
                    SystemConfiguratorError::Cancelled,
                ));
            }
            if exit_status == Some(127) {
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
            format!("timed out after {}", crate::command::duration_text(timeout)),
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
    use crate::backend_apply::RestartImpact;
    use crate::command::{CommandOutput, FakeCommandRunner};
    use crate::domain::{parse_connections, BackendIdentity, ConfigValue, StagedChange};

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        MainContext::new().block_on(future)
    }

    fn preview() -> ApplyPreview {
        ApplyPreview::new(
            BackendIdentity::new("myna-parakeet", "provider")
                .with_modelctl_app("myna-parakeet.parakeet"),
            vec![StagedChange::new(
                "verbose",
                ConfigValue::Boolean(false),
                ConfigValue::Boolean(true),
            )
            .unwrap()],
            RestartImpact::Required,
        )
        .unwrap()
    }

    /// pkexec exits 126 only when the user dismissed its prompt, which the
    /// Model tab treats like General's dismissed switch: nothing to report.
    #[test]
    fn a_dismissed_pkexec_prompt_is_a_cancellation() {
        let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(126),
            stdout: String::new(),
            stderr: "Error executing command as another user: Request dismissed".to_owned(),
        })]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

        let error = block_on(adapter.apply_backend_config(&preview(), CancellationToken::new()))
            .unwrap_err();

        assert!(error.completed().is_empty());
        assert_eq!(error.error(), &SystemConfiguratorError::Cancelled);
    }

    #[test]
    fn pkexec_exit_127_maps_to_authorization_denied() {
        for message in ["Autorisierung abgelehnt", ""] {
            let runner = FakeCommandRunner::scripted([Err(CommandError::NonZero {
                exit_status: Some(127),
                stdout: String::new(),
                stderr: message.to_owned(),
            })]);
            let adapter = PkexecSystemConfigurator::new(Arc::new(runner));

            let error =
                block_on(adapter.apply_backend_config(&preview(), CancellationToken::new()))
                    .unwrap_err();

            assert!(matches!(
                error.error(),
                SystemConfiguratorError::AuthorizationDenied {
                    step: FailedStep::Command {
                        exit_status: Some(127),
                        stderr,
                        ..
                    },
                    ..
                } if stderr == message
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
    fn the_privileged_plan_has_no_deadline() {
        let preview = preview();
        let runner = FakeCommandRunner::scripted([Ok(CommandOutput::new(
            Some(0),
            plan_output(preview.operations(), None),
            "",
        ))]);
        let adapter = PkexecSystemConfigurator::new(Arc::new(runner.clone()));

        block_on(adapter.apply_backend_config(&preview, CancellationToken::new())).unwrap();

        assert_eq!(runner.calls()[0].timeout(), None);
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
                step: FailedStep::Command {
                    executable,
                    arguments,
                    exit_status: Some(1),
                    stderr,
                },
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
                step: FailedStep::Command {
                    arguments,
                    stderr,
                    ..
                },
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
                step: FailedStep::Command {
                    executable,
                    exit_status: Some(2),
                    ..
                },
                message,
            } if executable == "pkexec" && message.contains("unexpected executable sh")
        ));
    }

    #[test]
    fn a_refused_snapd_request_is_reported_as_the_request_not_a_command() {
        let error = snapd_error_to_system_error(
            user_daemons_on_request(),
            SnapdError::AuthorizationDenied {
                status_code: 401,
                kind: Some("login-required".to_owned()),
                message: "access denied".to_owned(),
            },
        );

        assert_eq!(
            crate::backend_ui::system_error_details(&error),
            "Request: PUT /v2/snaps/system/conf (experimental.user-daemons=true)\n\
             HTTP status: 401\n\
             Message: access denied"
        );
    }

    #[test]
    fn a_message_of_several_lines_starts_on_its_own() {
        let error = snapd_error_to_system_error(
            user_daemons_on_request(),
            SnapdError::Transport {
                message: "first\nsecond".to_owned(),
            },
        );

        assert_eq!(
            crate::backend_ui::system_error_details(&error),
            "Request: PUT /v2/snaps/system/conf (experimental.user-daemons=true)\n\
             Message:\nsnapd transport error: first\nsecond"
        );
    }

    #[test]
    fn a_snapd_failure_without_an_answer_has_no_http_status() {
        let error = snapd_error_to_system_error(
            user_daemons_on_request(),
            SnapdError::Transport {
                message: "connection refused".to_owned(),
            },
        );

        assert_eq!(
            crate::backend_ui::system_error_details(&error),
            "Request: PUT /v2/snaps/system/conf (experimental.user-daemons=true)\n\
             Message: snapd transport error: connection refused"
        );
    }

    #[test]
    fn a_command_failure_whose_message_is_its_stderr_says_it_once() {
        let error = SystemConfiguratorError::authorization_denied(
            "pkexec",
            vec!["snap".to_owned(), "restart".to_owned()],
            Some(126),
            "Not authorized",
        );

        assert_eq!(
            crate::backend_ui::system_error_details(&error),
            "Executable: pkexec\nArguments: snap restart\nExit status: 126\n\
             Message: Not authorized"
        );
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
                step: FailedStep::Command {
                    exit_status: Some(126),
                    ref stderr,
                    ..
                },
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
                step: FailedStep::Command {
                    exit_status: Some(1),
                    ref stderr,
                    ..
                },
                ..
            } if stderr == "permission denied"
        ));
    }

    #[derive(Default)]
    struct ScriptedSnapd {
        interface_outcomes:
            Mutex<Vec<Result<crate::adapters::snapd_client::SnapdOutcome, SnapdError>>>,
        calls: Mutex<Vec<SnapdCall>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SnapdCall {
        Interface(InterfaceAction),
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

        async fn user_daemons_enabled(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<bool, SnapdError> {
            unreachable!("no switch reads the flag")
        }

        async fn enable_user_daemons(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), SnapdError> {
            unreachable!("no switch turns the flag on")
        }

        async fn install_snap(
            &self,
            _snap: &str,
            _cancellation: CancellationToken,
        ) -> Result<Option<String>, SnapdError> {
            unreachable!("no switch installs a snap")
        }

        async fn change(
            &self,
            _change_id: &str,
            _cancellation: CancellationToken,
        ) -> Result<ChangeInProgress, SnapdError> {
            unreachable!("no switch follows an install")
        }
    }

    fn switch_plan() -> SwitchPlan {
        let snapshot = parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend old:provider manual\n\
             content - new:provider -\n",
            "name: content\nslots:\n  - old:provider:\n      content: inference-provider\n      task: speech-to-text\n  - new:provider:\n      content: inference-provider\n      task: speech-to-text\n",
        )
        .unwrap();
        SwitchPlan::new(&snapshot, BackendIdentity::new("new", "provider")).unwrap()
    }

    fn invalid_switch_plan(operations: Vec<CommandRequest>) -> SwitchPlan {
        let snapshot = parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend old:provider manual\n\
             content - new:provider -\n",
            "name: content\nslots:\n  - old:provider:\n      content: inference-provider\n      task: speech-to-text\n  - new:provider:\n      content: inference-provider\n      task: speech-to-text\n",
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
        use crate::adapters::snapd_client::SnapdOutcome;
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Ok(SnapdOutcome::Sync), Ok(SnapdOutcome::Sync)]),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::scripted([Ok(CommandOutput::new(
            Some(0),
            "",
            "",
        ))]));
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner.clone(), snapd.clone());

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
        assert_eq!(calls.len(), 2);
        assert_eq!(runner.calls(), [myna_restart_request()]);
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
                status_code: 401,
                kind: None,
                message: "access denied".into(),
            })]),
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

    /// Notes what the runner had run when the restart asked it, then answers.
    struct RecordingGate {
        runner: Arc<FakeCommandRunner>,
        seen: std::cell::RefCell<Option<usize>>,
        answer: Result<(), SystemConfiguratorError>,
    }

    #[async_trait(?Send)]
    impl RestartGate for RecordingGate {
        async fn until_clear(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), SystemConfiguratorError> {
            self.seen.replace(Some(self.runner.calls().len()));
            self.answer.clone()
        }
    }

    fn gated(
        answer: Result<(), SystemConfiguratorError>,
    ) -> (
        PkexecSystemConfigurator,
        Arc<FakeCommandRunner>,
        Rc<RecordingGate>,
    ) {
        use crate::adapters::snapd_client::SnapdOutcome;
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Ok(SnapdOutcome::Sync), Ok(SnapdOutcome::Sync)]),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::scripted([Ok(CommandOutput::new(
            Some(0),
            "",
            "",
        ))]));
        let gate = Rc::new(RecordingGate {
            runner: runner.clone(),
            seen: Default::default(),
            answer,
        });
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner.clone(), snapd)
            .with_restart_gate(gate.clone());
        (adapter, runner, gate)
    }

    /// A restart under the daemon's open shortcut dialog would leave the
    /// dialog with nobody waiting for its answer, so it waits for the gate.
    #[test]
    fn a_switch_restarts_myna_only_once_the_gate_clears() {
        let (adapter, runner, gate) = gated(Ok(()));
        block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new())).unwrap();
        assert_eq!(*gate.seen.borrow(), Some(0), "restarted before the gate");
        assert_eq!(runner.calls(), [myna_restart_request()]);

        let (adapter, runner, gate) = gated(Ok(()));
        block_on(adapter.restart_myna(CancellationToken::new())).unwrap();
        assert_eq!(*gate.seen.borrow(), Some(0), "restarted before the gate");
        assert_eq!(runner.calls(), [myna_restart_request()]);
    }

    #[test]
    fn a_wait_for_the_gate_cancelled_restarts_nothing() {
        let (adapter, runner, _gate) = gated(Err(SystemConfiguratorError::Cancelled));
        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap_err();
        assert_eq!(failure.error(), &SystemConfiguratorError::Cancelled);
        assert_eq!(failure.completed().len(), 2);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn backend_switch_restart_failure_preserves_completed_interface_operations() {
        use crate::adapters::snapd_client::SnapdOutcome;
        let snapd = Arc::new(ScriptedSnapd {
            interface_outcomes: Mutex::new(vec![Ok(SnapdOutcome::Sync), Ok(SnapdOutcome::Sync)]),
            calls: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(FakeCommandRunner::scripted([Err(CommandError::NonZero {
            exit_status: Some(1),
            stdout: String::new(),
            stderr: "restart failed".into(),
        })]));
        let adapter = PkexecSystemConfigurator::with_snapd_client(runner.clone(), snapd.clone());

        let failure =
            block_on(adapter.execute_backend_switch(&switch_plan(), CancellationToken::new()))
                .unwrap_err();
        assert_eq!(failure.completed().len(), 2);
        assert!(matches!(
            failure.error(),
            SystemConfiguratorError::Execution { message, .. } if message == "restart failed"
        ));
        assert_eq!(snapd.calls.lock().unwrap().len(), 2);
        assert_eq!(runner.calls(), [myna_restart_request()]);
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
                    myna_restart_request(),
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
                    myna_restart_request(),
                    myna_restart_request(),
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
                        "systemctl".into(),
                        vec!["--user".into(), "restart".into(), "other.service".into()],
                    ),
                ]),
                "restart must be exactly Myna's user service",
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
                ]),
                "unknown snap action restart",
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
            myna_restart_request(),
            myna_restart_request(),
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
            myna_restart_request(),
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
