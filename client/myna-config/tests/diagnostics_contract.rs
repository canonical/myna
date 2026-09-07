use std::time::Duration;

use myna_config::diagnostics::{
    parse_snap_list, present_diagnostics, BackendDiagnostic, DiagnosticConnection, DiagnosticInput,
    OnboardingState, RefreshPolicy, RefreshReason, APP_REFRESH_PROCESS_BUDGET,
    BACKEND_REFRESH_PROCESS_BUDGET, NO_BACKEND_COMMAND, NO_MYNA_COMMAND,
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
fn only_discovered_backends_are_reported() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n\
             myna-unrelated  4.0  10  latest/stable  example  -\n",
        )
        .unwrap(),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            version: "3.0".into(),
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
        problems: vec!["Installed snaps: snapd unavailable".into()],
        ..DiagnosticInput::default()
    });
    assert_eq!(report.onboarding(), OnboardingState::Unavailable);
    assert_eq!(report.onboarding_command(), None);
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
    assert!(text.contains("community-asr - Multiple connections"));
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
        "Machine",
        "Daemon",
        "Backends",
        "Problems",
        "none selected",
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
        problems: vec!["Backend connections: discovery failed".into()],
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
