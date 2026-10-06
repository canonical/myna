//! The privileged half of a backend apply and of onboarding's set-up.
//!
//! Either is a short list of `snap` commands that must run as root. Running
//! each one through its own `pkexec` costs the user one authorization prompt
//! per command, because pkexec's polkit action carries no `keep`. Instead the
//! whole plan is serialized and handed to a single `pkexec myna-config
//! --apply-plan <json>` (or `--set-up <json>`) invocation; this module is
//! both the encoder the unprivileged UI uses and the executor that runs under
//! root.
//!
//! The executor trusts nothing about its input: a plan is accepted only when
//! every operation matches one of the exact shapes the UI can produce for its
//! kind. The polkit dialog shows the message of the kind's action in
//! `data/com.canonical.Myna.Config.policy` rather than the plan's argv.

use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::command::CommandRequest;
use crate::domain::CommandResult;

/// Command-line flag that runs a backend apply's plan.
pub const APPLY_PLAN_FLAG: &str = "--apply-plan";

/// Command-line flag that runs onboarding's set-up plan.
pub const SET_UP_FLAG: &str = "--set-up";

/// Which privileged plan the executor runs: each has its own polkit action,
/// so its own prompt message, and its own allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKind {
    /// A backend's settings: `modelctl` calls and a restart.
    Apply,
    /// snapd's `experimental.user-daemons` flag and installs of Myna's own
    /// snaps from edge.
    SetUp,
}

impl PlanKind {
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Apply => APPLY_PLAN_FLAG,
            Self::SetUp => SET_UP_FLAG,
        }
    }
}

const ALLOWED_EXECUTABLE: &str = "snap";
const MODELCTL_TAIL: [&str; 2] = ["--assume-yes", "--no-restart"];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlanOperation {
    executable: String,
    arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlanResult {
    executable: String,
    arguments: Vec<String>,
    exit_status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl From<PlanResult> for CommandResult {
    fn from(value: PlanResult) -> Self {
        CommandResult::new(
            value.executable,
            value.arguments,
            value.exit_status,
            value.stdout,
            value.stderr,
        )
    }
}

/// Exit status of the executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanExit {
    /// Every operation exited 0.
    Success,
    /// An operation failed; the results printed include it as the last entry.
    OperationFailed,
    /// The plan was rejected before anything ran; nothing was printed on stdout.
    InvalidPlan,
}

impl PlanExit {
    pub fn code(self) -> i32 {
        match self {
            PlanExit::Success => 0,
            PlanExit::OperationFailed => 1,
            PlanExit::InvalidPlan => 2,
        }
    }
}

/// Serialize a validated plan for the executor's argv.
pub fn encode_plan(kind: PlanKind, operations: &[CommandRequest]) -> Result<String, String> {
    let plan: Vec<PlanOperation> = operations
        .iter()
        .map(|request| PlanOperation {
            executable: request.executable().to_owned(),
            arguments: request.arguments().to_vec(),
        })
        .collect();
    validate_plan(kind, &plan)?;
    serde_json::to_string(&plan).map_err(|error| error.to_string())
}

/// Parse the executor's stdout back into per-operation results.
pub fn decode_results(stdout: &str) -> Result<Vec<CommandResult>, String> {
    let results: Vec<PlanResult> =
        serde_json::from_str(stdout).map_err(|error| error.to_string())?;
    Ok(results.into_iter().map(CommandResult::from).collect())
}

/// Executor entry point: validate, run each operation in order, stop at the
/// first failure, and print the results as JSON on stdout.
pub fn run_plan(kind: PlanKind, json: &str) -> PlanExit {
    let plan: Vec<PlanOperation> = match serde_json::from_str(json) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("myna-config: invalid apply plan: {error}");
            return PlanExit::InvalidPlan;
        }
    };
    if let Err(message) = validate_plan(kind, &plan) {
        eprintln!("myna-config: invalid apply plan: {message}");
        return PlanExit::InvalidPlan;
    }

    let mut results = Vec::with_capacity(plan.len());
    let mut exit = PlanExit::Success;
    for operation in plan {
        let result = execute(&operation);
        let failed = result.exit_status != Some(0);
        results.push(result);
        if failed {
            exit = PlanExit::OperationFailed;
            break;
        }
    }
    match serde_json::to_string(&results) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("myna-config: cannot serialize apply results: {error}");
            return PlanExit::InvalidPlan;
        }
    }
    exit
}

fn execute(operation: &PlanOperation) -> PlanResult {
    let output = Command::new(&operation.executable)
        .args(&operation.arguments)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) => PlanResult {
            executable: operation.executable.clone(),
            arguments: operation.arguments.clone(),
            exit_status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(error) => PlanResult {
            executable: operation.executable.clone(),
            arguments: operation.arguments.clone(),
            exit_status: None,
            stdout: String::new(),
            stderr: format!("cannot run {}: {error}", operation.executable),
        },
    }
}

/// Accept only the exact command shapes a plan of `kind` can contain.
pub fn validate_plan(kind: PlanKind, plan: &[PlanOperation]) -> Result<(), String> {
    if plan.is_empty() {
        return Err("plan has no operations".to_owned());
    }
    for operation in plan {
        if operation.executable != ALLOWED_EXECUTABLE {
            return Err(format!("unexpected executable {}", operation.executable));
        }
        let args: Vec<&str> = operation.arguments.iter().map(String::as_str).collect();
        match kind {
            PlanKind::Apply => validate_apply(&args),
            PlanKind::SetUp => validate_set_up(&args),
        }?;
    }
    Ok(())
}

/// The flag, installs of Myna and the known backends, and Myna's backend
/// plug connected to one of theirs: the prompt says it sets Myna up, so it
/// touches nothing else.
fn validate_set_up(args: &[&str]) -> Result<(), String> {
    let is_mynas = |snap: &str| {
        snap == crate::onboarding::MYNA_SNAP
            || myna_core::language::ModelFamily::from_snap_name(snap).is_some()
    };
    match args {
        ["set", "system", "experimental.user-daemons=true"] => Ok(()),
        ["install", "--edge", snap] if is_mynas(snap) => Ok(()),
        ["connect", "myna:backend", slot] => match slot.strip_suffix(":provider") {
            Some(snap) if myna_core::language::ModelFamily::from_snap_name(snap).is_some() => {
                Ok(())
            }
            _ => Err(format!("{slot} is not a model's provider slot")),
        },
        ["install", "--edge", snap] => Err(format!("{snap} is not one of Myna's snaps")),
        _ => Err(unexpected(args)),
    }
}

fn validate_apply(args: &[&str]) -> Result<(), String> {
    match args {
        ["restart", snap] => {
            if !is_snap_name(snap) {
                return Err(format!("invalid snap name {snap}"));
            }
            Ok(())
        }
        ["run", app, "set", rest @ ..] => {
            if !is_modelctl_app(app) {
                return Err(format!("invalid modelctl app {app}"));
            }
            let Some(assignments) = rest.strip_suffix(&MODELCTL_TAIL) else {
                return Err("modelctl set must end with --assume-yes --no-restart".to_owned());
            };
            if assignments.is_empty() {
                return Err("modelctl set has no assignments".to_owned());
            }
            for assignment in assignments {
                let Some((key, _)) = assignment.split_once('=') else {
                    return Err(format!("assignment {assignment} is not key=value"));
                };
                if !is_config_key(key) {
                    return Err(format!("invalid configuration key {key}"));
                }
            }
            Ok(())
        }
        ["run", app, "use-model", model, tail @ ..] => {
            if !is_modelctl_app(app) {
                return Err(format!("invalid modelctl app {app}"));
            }
            if model.is_empty() || model.starts_with('-') {
                return Err(format!("invalid model {model}"));
            }
            if tail != MODELCTL_TAIL {
                return Err("use-model must end with --assume-yes --no-restart".to_owned());
            }
            Ok(())
        }
        ["run", app, "use-engine", engine, tail @ ..] => {
            if !is_modelctl_app(app) {
                return Err(format!("invalid modelctl app {app}"));
            }
            if engine.is_empty() || (engine.starts_with('-') && *engine != "--auto") {
                return Err(format!("invalid engine {engine}"));
            }
            if tail != MODELCTL_TAIL {
                return Err("use-engine must end with --assume-yes --no-restart".to_owned());
            }
            Ok(())
        }
        _ => Err(unexpected(args)),
    }
}

fn unexpected(args: &[&str]) -> String {
    format!(
        "unexpected operation: {ALLOWED_EXECUTABLE} {}",
        args.join(" ")
    )
}

fn is_snap_name(name: &str) -> bool {
    crate::adapters::snapd_client::is_valid_snap_name(name)
}

fn is_modelctl_app(app: &str) -> bool {
    match app.split_once('.') {
        Some((snap, command)) => is_snap_name(snap) && is_snap_name(command),
        None => is_snap_name(app),
    }
}

fn is_config_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Test-only builder for the executor's stdout: one result per operation up
/// to and including `failed`, which exits 1 with the given stderr.
#[cfg(test)]
pub(crate) fn plan_output(operations: &[CommandRequest], failed: Option<(usize, &str)>) -> String {
    let mut results = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        let (exit_status, stderr) = match failed {
            Some((failed_index, stderr)) if failed_index == index => (1, stderr),
            _ => (0, ""),
        };
        results.push(PlanResult {
            executable: operation.executable().to_owned(),
            arguments: operation.arguments().to_vec(),
            exit_status: Some(exit_status),
            stdout: String::new(),
            stderr: stderr.to_owned(),
        });
        if exit_status != 0 {
            break;
        }
    }
    serde_json::to_string(&results).expect("plan results serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(arguments: &[&str]) -> PlanOperation {
        PlanOperation {
            executable: "snap".to_owned(),
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[test]
    fn accepts_every_shape_the_ui_produces() {
        let plan = vec![
            op(&[
                "run",
                "myna-whisper.whisper",
                "set",
                "streaming=false",
                r#"alpha=" spaced ; $(rm -rf /) ""#,
                "--assume-yes",
                "--no-restart",
            ]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "use-model",
                "small",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&[
                "run",
                "myna-whisper",
                "use-engine",
                "--auto",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&["restart", "myna-whisper"]),
        ];
        assert_eq!(validate_plan(PlanKind::Apply, &plan), Ok(()));
    }

    #[test]
    fn rejects_anything_outside_the_allowlist() {
        let rejected = [
            PlanOperation {
                executable: "sh".to_owned(),
                arguments: vec!["-c".to_owned(), "id".to_owned()],
            },
            op(&["set", "myna-whisper", "streaming=false"]),
            op(&["restart", "myna-whisper", "--reload"]),
            op(&["restart", "../etc"]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "set",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&["run", "myna-whisper.whisper", "set", "streaming=false"]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "set",
                "--verbose=1",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "set",
                "novalue",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "use-model",
                "--all",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&[
                "run",
                "myna-whisper.whisper",
                "use-engine",
                "--list",
                "--assume-yes",
                "--no-restart",
            ]),
            op(&["run", "myna-whisper.whisper", "run", "--", "id"]),
            op(&[
                "run",
                "Bad Name",
                "use-model",
                "x",
                "--assume-yes",
                "--no-restart",
            ]),
        ];
        for operation in rejected {
            assert!(
                validate_plan(PlanKind::Apply, std::slice::from_ref(&operation)).is_err(),
                "{operation:?}"
            );
        }
        assert!(validate_plan(PlanKind::Apply, &[]).is_err());
    }

    #[test]
    fn set_up_touches_only_the_flag_and_mynas_snaps() {
        let plan = vec![
            op(&["set", "system", "experimental.user-daemons=true"]),
            op(&["install", "--edge", "myna"]),
            op(&["install", "--edge", "myna-parakeet"]),
            op(&["install", "--edge", "myna-whisper"]),
            op(&["install", "--edge", "myna-funasr"]),
            op(&["connect", "myna:backend", "myna-parakeet:provider"]),
            op(&["connect", "myna:backend", "myna-whisper:provider"]),
        ];
        assert_eq!(validate_plan(PlanKind::SetUp, &plan), Ok(()));

        let rejected = [
            op(&["install", "--edge", "firefox"]),
            op(&["install", "myna"]),
            op(&["install", "--edge", "--dangerous", "myna"]),
            op(&["install", "--edge", "myna", "myna-parakeet"]),
            op(&["set", "system", "experimental.user-daemons=false"]),
            op(&["set", "system", "proxy.http=http://evil"]),
            op(&["restart", "myna-whisper"]),
            op(&["connect", "myna:backend", "myna:provider"]),
            op(&["connect", "myna:backend", "gemma4:provider"]),
            op(&["connect", "myna:backend", "myna-parakeet:other"]),
            op(&["connect", "myna:pipewire", "myna-parakeet:provider"]),
            op(&["disconnect", "myna:backend", "myna-parakeet:provider"]),
        ];
        for operation in rejected {
            assert!(
                validate_plan(PlanKind::SetUp, std::slice::from_ref(&operation)).is_err(),
                "{operation:?}"
            );
        }
        // Neither kind accepts the other's operations.
        assert!(validate_plan(PlanKind::Apply, &plan[..1]).is_err());
        assert!(validate_plan(PlanKind::Apply, &plan[1..2]).is_err());
    }

    #[test]
    fn plan_round_trips_through_json() {
        let operations = vec![
            CommandRequest::new(
                "snap".to_owned(),
                [
                    "run",
                    "myna-whisper.whisper",
                    "set",
                    "streaming=false",
                    "--assume-yes",
                    "--no-restart",
                ]
                .map(str::to_owned)
                .to_vec(),
            ),
            CommandRequest::new(
                "snap".to_owned(),
                ["restart", "myna-whisper"].map(str::to_owned).to_vec(),
            ),
        ];
        let json = encode_plan(PlanKind::Apply, &operations).unwrap();
        let decoded: Vec<PlanOperation> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded,
            vec![
                op(&[
                    "run",
                    "myna-whisper.whisper",
                    "set",
                    "streaming=false",
                    "--assume-yes",
                    "--no-restart"
                ]),
                op(&["restart", "myna-whisper"]),
            ]
        );
        assert!(encode_plan(
            PlanKind::Apply,
            &[CommandRequest::new("pkexec".to_owned(), vec![])]
        )
        .is_err());
    }

    #[test]
    fn results_decode_into_command_results() {
        let results = decode_results(
            r#"[{"executable":"snap","arguments":["restart","myna-whisper"],"exit_status":0,"stdout":"","stderr":""}]"#,
        )
        .unwrap();
        assert_eq!(
            results,
            vec![CommandResult::new(
                "snap",
                ["restart", "myna-whisper"].map(str::to_owned).to_vec(),
                Some(0),
                "",
                ""
            )]
        );
        assert!(decode_results("not json").is_err());
    }

    /// The policy's action for `kind`, from its `<action` to its `</action>`.
    fn policy_action(kind: PlanKind) -> &'static str {
        let policy = include_str!("../data/com.canonical.Myna.Config.policy");
        let argv1 = format!(
            r#"<annotate key="org.freedesktop.policykit.exec.argv1">{}</annotate>"#,
            kind.flag()
        );
        let end = policy.find(&argv1).expect("an action for the flag");
        let start = policy[..end].rfind("<action ").expect("its opening");
        &policy[start..end]
    }

    #[test]
    fn each_plan_kind_has_its_own_polkit_action() {
        assert!(policy_action(PlanKind::Apply).contains("change speech-to-text model settings"));
        assert!(policy_action(PlanKind::SetUp).contains("set up Myna"));
    }

    #[test]
    fn only_the_active_session_keeps_its_authorization() {
        for kind in [PlanKind::Apply, PlanKind::SetUp] {
            let action = policy_action(kind);
            for default in [
                "<allow_any>auth_admin</allow_any>",
                "<allow_inactive>auth_admin</allow_inactive>",
                "<allow_active>auth_admin_keep</allow_active>",
            ] {
                assert!(action.contains(default), "{kind:?}: {default} missing");
            }
        }
    }
}
