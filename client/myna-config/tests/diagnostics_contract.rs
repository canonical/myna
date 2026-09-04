use std::time::Duration;

use myna_config::diagnostics::{
    parse_snap_list, present_diagnostics, BackendDiagnostic, DiagnosticConnection,
    DiagnosticFailure, DiagnosticInput, OnboardingState, RefreshPolicy, RefreshReason,
    APP_REFRESH_PROCESS_BUDGET, BACKEND_REFRESH_PROCESS_BUDGET, NO_BACKEND_COMMAND,
    NO_MYNA_COMMAND,
};

#[test]
fn presenter_distinguishes_no_myna_and_no_backend_onboarding() {
    let no_myna = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        ..DiagnosticInput::default()
    });
    assert_eq!(no_myna.onboarding(), OnboardingState::NoMyna);
    assert_eq!(no_myna.onboarding_command(), Some(NO_MYNA_COMMAND));

    let no_backend = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\nmyna  1.2.3  7  latest/stable  canonical**  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        ..DiagnosticInput::default()
    });
    assert_eq!(no_backend.onboarding(), OnboardingState::NoBackend);
    assert_eq!(no_backend.onboarding_command(), Some(NO_BACKEND_COMMAND));
}

#[test]
fn onboarding_uses_discovered_backend_identities_not_snap_name_prefixes() {
    let installed = |rows| parse_snap_list(rows).unwrap();
    let myna_and_unrelated = installed(
        "Name  Version  Rev  Tracking  Publisher  Notes\n\
         myna  1.2.3  7  latest/stable  canonical**  -\n\
         myna-not-a-backend  4.0  8  latest/stable  example  -\n",
    );

    let no_backend = present_diagnostics(DiagnosticInput {
        installed_snaps: myna_and_unrelated.clone(),
        inventory_complete: true,
        ..DiagnosticInput::default()
    });
    assert_eq!(no_backend.onboarding(), OnboardingState::NoBackend);

    let discovered_backend = present_diagnostics(DiagnosticInput {
        installed_snaps: myna_and_unrelated,
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(discovered_backend.onboarding(), OnboardingState::Ready);

    let missing_myna = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             community-asr  1.0  3  latest/stable  example  -\n",
        ),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(missing_myna.onboarding(), OnboardingState::NoMyna);

    let incomplete = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n",
        ),
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(incomplete.onboarding(), OnboardingState::Unavailable);
}

#[test]
fn installed_versions_include_discovered_non_prefixed_backends_only() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n\
             community-asr  3.0  9  latest/stable  example  -\n\
             myna-unrelated  4.0  10  latest/stable  example  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });

    let text = report.copy_text();
    assert!(text.contains("community-asr 3.0"));
    assert!(!text.contains("myna-unrelated"));
}

#[test]
fn inventory_error_does_not_claim_myna_is_absent() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_failure: Some(DiagnosticFailure {
            message: "snapd unavailable".into(),
            ..DiagnosticFailure::default()
        }),
        ..DiagnosticInput::default()
    });
    assert_eq!(report.onboarding(), OnboardingState::Unavailable);
    assert_eq!(report.onboarding_command(), None);
}

#[test]
fn report_contains_versions_resolution_connections_and_failures() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n\
             myna-parakeet  2.0  8  latest/stable  canonical**  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "myna-parakeet".into(),
            modelctl_app: Some("myna-parakeet.modelctl".into()),
            connection: DiagnosticConnection::Connected,
            failures: vec![DiagnosticFailure {
                surface: "status".into(),
                executable: "snap".into(),
                arguments: vec![
                    "run".into(),
                    "myna-parakeet.modelctl".into(),
                    "status".into(),
                ],
                message: "command failed".into(),
                stderr: "arbitrary dictated words".into(),
            }],
        }],
        ..DiagnosticInput::default()
    });
    let text = report.copy_text();
    assert!(text.contains(concat!("Myna Settings ", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains("myna 1.2.3"));
    assert!(text.contains("myna-parakeet 2.0"));
    assert!(text.contains("myna-parakeet.modelctl"));
    assert!(text.contains("Connected"));
    assert!(text.contains("command failed"));
    assert!(!text.contains("arbitrary dictated words"));
}

#[test]
fn diagnostics_remove_sensitive_content_and_redact_paths_and_secret_values() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        inventory_failure: Some(DiagnosticFailure {
            surface: "inventory".into(),
            executable: "/home/alice/bin/snap".into(),
            arguments: vec![
                "list".into(),
                "--token=top-secret".into(),
                "/home/alice/private".into(),
            ],
            message: "failed at /home/alice/private".into(),
            stderr: "transcript=private words\naudio=/home/alice/voice.wav\nsocket=/run/user/1000/myna.sock".into(),
        }),
        ..DiagnosticInput::default()
    });
    let text = report.copy_text();
    for forbidden in [
        "private words",
        "voice.wav",
        "top-secret",
        "/home/alice",
        "/run/user/1000",
    ] {
        assert!(!text.contains(forbidden), "leaked {forbidden}: {text}");
    }
    assert!(text.contains("[redacted]"));
    assert!(text.contains("<path>"));
}

#[test]
fn split_secret_arguments_redact_the_following_value_only() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        inventory_failure: Some(DiagnosticFailure {
            executable: "backend-tool".into(),
            arguments: vec![
                "--token".into(),
                "top-secret".into(),
                "--mode".into(),
                "offline".into(),
                "--password".into(),
                "hunter2".into(),
                "--api-key=embedded-secret".into(),
                "--credential".into(),
            ],
            message: "failed".into(),
            ..DiagnosticFailure::default()
        }),
        ..DiagnosticInput::default()
    });

    let text = report.copy_text();
    for secret in ["top-secret", "hunter2", "embedded-secret"] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    assert!(text.contains("--token [redacted]"));
    assert!(text.contains("--password [redacted]"));
    assert!(text.contains("--api-key=[redacted]"));
    assert!(text.contains("--mode offline"));
    assert!(
        text.contains("--credential [redacted]"),
        "an end-of-list secret flag must still show that its value was redacted: {text}"
    );
}

#[test]
fn adjacent_secret_flags_cannot_expose_the_later_value() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        inventory_failure: Some(DiagnosticFailure {
            executable: "backend-tool".into(),
            arguments: vec!["--token".into(), "--password".into(), "hunter2".into()],
            ..DiagnosticFailure::default()
        }),
        ..DiagnosticInput::default()
    });

    let text = report.copy_text();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("--token [redacted] --password [redacted]"));
}

#[test]
fn sensitive_assignment_arguments_redact_the_entire_value_including_whitespace() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        inventory_failure: Some(DiagnosticFailure {
            executable: "backend-tool".into(),
            arguments: vec![
                "--token=correct horse".into(),
                "--api-key=\"quoted secret\" and more".into(),
                "--password=first\tsecond\nthird".into(),
            ],
            ..DiagnosticFailure::default()
        }),
        ..DiagnosticInput::default()
    });

    let text = report.copy_text();
    for secret in [
        "correct horse",
        "horse",
        "quoted secret",
        "and more",
        "first",
        "second",
        "third",
    ] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    for argument in [
        "--token=[redacted]",
        "--api-key=[redacted]",
        "--password=[redacted]",
    ] {
        assert!(
            text.contains(argument),
            "missing redaction for {argument}: {text}"
        );
    }
}

#[test]
fn snap_list_parser_accepts_empty_and_reports_malformed_rows() {
    assert!(
        parse_snap_list("No snaps are installed yet. Try 'snap install hello-world'.\n")
            .unwrap()
            .is_empty()
    );
    assert!(parse_snap_list("Name Version\nmyna\n").is_err());
}

#[test]
fn diagnostics_status_values_are_translatable_user_facing_labels() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            connection: DiagnosticConnection::MultipleConnections,
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    let text = report.copy_text();
    assert!(text.contains("Onboarding: Ready"));
    assert!(text.contains("Connection: Multiple connections"));
    assert!(!text.contains("no-backend"));
    assert!(!text.contains("contested"));

    let pot = include_str!("../po/myna-config.pot");
    for label in [
        "Myna is not installed",
        "No backend discovered",
        "Ready",
        "Installation status unavailable",
        "Connected",
        "Multiple connections",
        "Not connected",
        "Installed snaps",
        "Backend connections",
        "Model control command",
        "Backend configuration",
        "Backend status",
        "Available models",
        "Available engines",
    ] {
        assert!(pot.contains(&format!("msgid \"{label}\"")), "{label}");
    }
}

#[test]
fn failed_backend_discovery_does_not_claim_onboarding_is_complete() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        failures: vec![DiagnosticFailure {
            surface: "Backend connections".into(),
            message: "discovery failed".into(),
            ..DiagnosticFailure::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(report.onboarding(), OnboardingState::Unavailable);
    assert_eq!(report.onboarding_command(), None);
}

#[test]
fn refresh_policy_has_no_idle_poll_and_enforces_process_budgets() {
    let policy = RefreshPolicy::default();
    assert_eq!(policy.plan(RefreshReason::Idle, 4).processes(), 0);
    assert_eq!(policy.periodic_interval(), None);
    assert!(policy.plan(RefreshReason::Startup, 4).processes() <= APP_REFRESH_PROCESS_BUDGET);
    assert!(
        policy.plan(RefreshReason::BackendSelected, 1).processes()
            <= BACKEND_REFRESH_PROCESS_BUDGET
    );
    assert_eq!(
        policy
            .plan(RefreshReason::DiagnosticsRequested, 3)
            .processes(),
        APP_REFRESH_PROCESS_BUDGET + 3 * BACKEND_REFRESH_PROCESS_BUDGET
    );
    assert_eq!(policy.debounce(), Duration::from_millis(250));
}

#[test]
fn sensitive_assignments_are_redacted_even_when_embedded_in_messages() {
    let report = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        inventory_failure: Some(DiagnosticFailure {
            message: "command failed: transcript=private words audio=/home/me/voice.wav".into(),
            stderr: "prefix transcript=other private words".into(),
            ..DiagnosticFailure::default()
        }),
        ..DiagnosticInput::default()
    });
    let text = report.copy_text();
    assert!(!text.contains("private words"));
    assert!(!text.contains("other private words"));
    assert!(!text.contains("voice.wav"));
    assert!(!text.contains("prefix transcript"));
}
