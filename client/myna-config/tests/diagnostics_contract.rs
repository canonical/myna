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

mod performance_warnings {
    use myna_config::diagnostics::{present_diagnostics, DiagnosticInput, InstalledSnap};
    use myna_config::performance::{
        ClockClass, ClockFacts, PerformanceFacts, PowerFacts, Pressure,
    };

    fn class(cpu: u32, hardware: u64, policy: u64, achieved: Option<u64>) -> ClockClass {
        ClockClass {
            cpu,
            cores: 8,
            hardware_max_khz: hardware,
            policy_max_khz: policy,
            min_khz: 623_377,
            achieved_khz: achieved,
        }
    }

    fn ready_input(performance: Option<PerformanceFacts>) -> DiagnosticInput {
        DiagnosticInput {
            inventory_complete: true,
            installed_snaps: vec![InstalledSnap {
                name: "myna".into(),
                version: "0.1.0".into(),
            }],
            performance,
            ..DiagnosticInput::default()
        }
    }

    #[test]
    fn a_firmware_clamp_is_a_warning_with_a_remedy_and_does_not_block_ready() {
        let report = present_diagnostics(ready_input(Some(PerformanceFacts {
            clock: ClockFacts {
                classes: vec![
                    class(0, 5_090_910, 5_090_910, Some(858_836)),
                    class(1, 3_506_494, 3_506_494, Some(602_579)),
                ],
                power: PowerFacts {
                    platform_profile: Some("balanced".into()),
                    on_mains: Some(true),
                    battery_status: Some("Not charging".into()),
                },
            },
            pressure: Some(Pressure::default()),
        })));
        let text = report.copy_text();

        assert_eq!(report.warnings().len(), 1, "{text}");
        let warning = &report.warnings()[0];
        assert!(
            warning
                .cause
                .contains("Firmware is holding the CPU at its lowest clock"),
            "{}",
            warning.cause
        );
        assert!(
            warning.cause.contains("cpu0 reached 0.86 GHz of 5.09 GHz"),
            "{}",
            warning.cause
        );
        assert!(
            warning.remedy.contains("Unplug the charger"),
            "{}",
            warning.remedy
        );
        assert!(text.contains("Warnings:\n  Firmware is holding"), "{text}");
        assert!(
            text.contains("Clock      cpu0 reached 0.86 GHz of 5.09 GHz, 8 cores in this class"),
            "{text}"
        );
        assert!(text.contains("Profile    balanced"), "{text}");
        assert!(
            text.contains("Power      mains, battery not charging"),
            "{text}"
        );
        assert!(text.contains("Pressure   cpu 0.00%"), "{text}");
        // A warning is not a problem: the machine is set up.
        assert!(text.contains("Problems:\n  (none)"), "{text}");
        assert_ne!(
            report.onboarding(),
            myna_config::diagnostics::OnboardingState::Unavailable
        );
        assert!(!text.contains('/'), "{text}");
    }

    #[test]
    fn a_software_cap_names_the_policy_file_and_both_ceilings() {
        let report = present_diagnostics(ready_input(Some(PerformanceFacts {
            clock: ClockFacts {
                classes: vec![class(0, 5_090_910, 1_500_000, Some(1_480_000))],
                power: PowerFacts::default(),
            },
            pressure: None,
        })));
        let warning = &report.warnings()[0];
        assert!(
            warning.cause.contains("capped by system policy"),
            "{}",
            warning.cause
        );
        assert!(
            warning.cause.contains("1.50 GHz of 5.09 GHz"),
            "{}",
            warning.cause
        );
        assert!(
            warning.remedy.contains("scaling_max_freq"),
            "{}",
            warning.remedy
        );
        assert!(
            report.copy_text().contains("(policy allows 1.50 GHz)"),
            "{}",
            report.copy_text()
        );
    }

    #[test]
    fn a_low_power_profile_points_at_the_power_settings() {
        let report = present_diagnostics(ready_input(Some(PerformanceFacts {
            clock: ClockFacts {
                classes: vec![class(0, 5_090_910, 5_090_910, Some(1_200_000))],
                power: PowerFacts {
                    platform_profile: Some("low-power".into()),
                    ..PowerFacts::default()
                },
            },
            pressure: None,
        })));
        let warning = &report.warnings()[0];
        assert!(warning.cause.contains("low-power"), "{}", warning.cause);
        assert!(
            warning.remedy.contains("Balanced or Performance"),
            "{}",
            warning.remedy
        );
    }

    #[test]
    fn pressure_warnings_stand_on_their_own() {
        let report = present_diagnostics(ready_input(Some(PerformanceFacts {
            clock: ClockFacts {
                classes: vec![class(0, 5_090_910, 5_090_910, Some(4_900_000))],
                power: PowerFacts::default(),
            },
            pressure: Some(Pressure {
                cpu_some: 0,
                memory_some: 42_10,
                io_full: 15_00,
            }),
        })));
        let causes: Vec<&str> = report
            .warnings()
            .iter()
            .map(|warning| warning.cause.as_str())
            .collect();
        assert_eq!(causes.len(), 2, "{causes:?}");
        assert!(
            causes[0].starts_with("The system is short of memory (42.10%"),
            "{causes:?}"
        );
        assert!(
            causes[1].starts_with("Disk activity is stalling the system (15.00%"),
            "{causes:?}"
        );
    }

    #[test]
    fn healthy_unknown_and_unmeasured_hosts_raise_nothing() {
        let healthy = present_diagnostics(ready_input(Some(PerformanceFacts {
            clock: ClockFacts {
                classes: vec![class(0, 5_090_910, 5_090_910, Some(4_900_000))],
                power: PowerFacts::default(),
            },
            pressure: Some(Pressure::default()),
        })));
        assert!(healthy.warnings().is_empty());
        assert!(healthy.copy_text().contains("Warnings:\n  (none)"));

        let unknown = present_diagnostics(ready_input(Some(PerformanceFacts::default())));
        assert!(unknown.warnings().is_empty());
        assert!(
            unknown
                .copy_text()
                .contains("Clock      (no cpufreq information)"),
            "{}",
            unknown.copy_text()
        );

        let unmeasured = present_diagnostics(ready_input(None));
        assert!(unmeasured.warnings().is_empty());
        assert!(
            unmeasured
                .copy_text()
                .contains("Clock      (not measured yet)"),
            "{}",
            unmeasured.copy_text()
        );
    }
}
