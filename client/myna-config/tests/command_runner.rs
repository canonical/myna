use std::collections::BTreeMap;
use std::time::Duration;

use myna_config::command::{
    CancellationToken, CommandError, CommandOutput, CommandRequest, CommandRunner,
    FakeCommandRunner, GioCommandRunner, OutputStream,
};

fn fixture() -> String {
    env!("CARGO_BIN_EXE_command-runner-fixture").to_owned()
}

fn request(arguments: &[&str]) -> CommandRequest {
    CommandRequest::new(
        fixture(),
        arguments.iter().map(|value| (*value).to_owned()).collect(),
    )
    .with_timeout(Duration::from_secs(2))
}

fn run(
    request: CommandRequest,
    cancellation: CancellationToken,
) -> Result<CommandOutput, CommandError> {
    gtk4::glib::MainContext::new().block_on(GioCommandRunner.run(request, cancellation))
}

#[test]
fn runs_from_glib_main_context_without_tokio_runtime() {
    let output = run(request(&["from-glib"]), CancellationToken::new()).unwrap();

    assert_eq!(output.stdout(), "\nfrom-glib\n");
}

#[test]
fn preserves_arguments_without_shell_interpretation_and_applies_environment() {
    let arguments = [
        "argument with spaces",
        "$(not-a-command)",
        "semi;colon",
        "*.txt",
    ];
    let request = request(&arguments).with_environment(BTreeMap::from([(
        "COMMAND_RUNNER_TEST".into(),
        "environment value".into(),
    )]));

    let output = run(request, CancellationToken::new()).unwrap();

    assert_eq!(output.exit_status(), Some(0));
    assert_eq!(
        output.stdout(),
        "environment value\nargument with spaces\n$(not-a-command)\nsemi;colon\n*.txt\n"
    );
    assert_eq!(output.stderr(), "");
}

#[test]
fn reports_non_zero_exit_with_captured_output() {
    let error = run(request(&["--fail"]), CancellationToken::new()).unwrap_err();

    assert!(matches!(
        error,
        CommandError::NonZero {
            exit_status: Some(23),
            stdout,
            stderr,
        } if stdout == "failure stdout\n" && stderr == "failure stderr\n"
    ));
}

#[test]
fn times_out_a_running_process() {
    let error = run(
        request(&["--sleep"]).with_timeout(Duration::from_millis(20)),
        CancellationToken::new(),
    )
    .unwrap_err();

    assert!(matches!(error, CommandError::Timeout { .. }));
}

#[test]
fn cancellation_is_distinct_from_timeout() {
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        trigger.cancel();
    });

    let error = run(
        request(&["--sleep"]).with_timeout(Duration::from_secs(2)),
        cancellation,
    )
    .unwrap_err();

    assert_eq!(error, CommandError::Cancelled);
}

#[test]
fn distinguishes_not_found_spawn_and_invalid_utf8_errors() {
    let missing = CommandRequest::new("/definitely/not/a/myna-command".into(), Vec::new());
    assert!(matches!(
        run(missing, CancellationToken::new()),
        Err(CommandError::NotFound { .. })
    ));

    let unexecutable = CommandRequest::new("/".into(), Vec::new());
    assert!(matches!(
        run(unexecutable, CancellationToken::new()),
        Err(CommandError::Spawn { .. })
    ));

    assert!(matches!(
        run(request(&["--invalid-stdout"]), CancellationToken::new()),
        Err(CommandError::InvalidUtf8 {
            stream: OutputStream::Stdout,
            ..
        })
    ));
}

#[test]
fn fake_runner_records_calls_and_returns_scripted_outcomes() {
    let success = CommandOutput::new(Some(0), "first", "");
    let fake = FakeCommandRunner::scripted([Ok(success.clone()), Err(CommandError::Cancelled)]);
    let first = CommandRequest::new("snap".into(), vec!["get".into(), "backend".into()]);
    let second = CommandRequest::new("modelctl".into(), vec!["status".into()]);

    let context = gtk4::glib::MainContext::new();
    assert_eq!(
        context.block_on(fake.run(first.clone(), CancellationToken::new())),
        Ok(success)
    );
    assert_eq!(
        context.block_on(fake.run(second.clone(), CancellationToken::new())),
        Err(CommandError::Cancelled)
    );
    assert_eq!(fake.calls(), vec![first, second]);
}
