use myna_config::{parse_args, Command, APP_ID, GETTEXT_DOMAIN};

#[test]
fn application_identity_is_stable() {
    assert_eq!(APP_ID, "com.canonical.Myna.Config");
    assert_eq!(GETTEXT_DOMAIN, "myna-config");
}

#[test]
fn no_arguments_launches_the_application() {
    assert_eq!(parse_args(Vec::<String>::new()).unwrap(), Command::Launch);
}

#[test]
fn informational_arguments_are_recognized() {
    assert_eq!(
        parse_args(["--help".to_string()]).unwrap(),
        Command::PrintHelp
    );
    assert_eq!(parse_args(["-h".to_string()]).unwrap(), Command::PrintHelp);
    assert_eq!(
        parse_args(["--version".to_string()]).unwrap(),
        Command::PrintVersion
    );
}

#[test]
fn unknown_arguments_are_rejected() {
    let error = parse_args(["--unknown".to_string()]).unwrap_err();
    assert!(error.contains("--unknown"));
    assert!(error.contains("Usage:"));
}
