use myna_config::diagnostics::{
    present_diagnostics, BackendDiagnostic, DiagnosticConnection, DiagnosticInput, InstalledSnap,
    OnboardingState,
};

fn installed(snaps: &[(&str, &str)]) -> Vec<InstalledSnap> {
    snaps
        .iter()
        .map(|(name, version)| InstalledSnap {
            name: (*name).to_owned(),
            version: (*version).to_owned(),
            ..Default::default()
        })
        .collect()
}

/// Whether a field line labelled `label` has a value starting with `value`,
/// however wide the label column is.
fn has_field(text: &str, label: &str, value: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .strip_prefix(label)
            .filter(|rest| rest.starts_with(' '))
            .is_some_and(|rest| rest.trim_start().starts_with(value))
    })
}

#[test]
fn presenter_distinguishes_no_myna_and_no_backend_onboarding() {
    let no_myna = present_diagnostics(DiagnosticInput {
        inventory_complete: true,
        ..DiagnosticInput::default()
    });
    assert_eq!(no_myna.onboarding(), OnboardingState::NoMyna);

    let no_backend = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(&[("myna", "1.2.3")]),
        inventory_complete: true,
        ..DiagnosticInput::default()
    });
    assert_eq!(no_backend.onboarding(), OnboardingState::NoBackend);
}

#[test]
fn onboarding_uses_discovered_backend_identities_not_snap_name_prefixes() {
    let myna_and_unrelated = installed(&[("myna", "1.2.3"), ("myna-not-a-backend", "4.0")]);

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
            connection: DiagnosticConnection::Connected,
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(discovered_backend.onboarding(), OnboardingState::Ready);

    let missing_myna = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(&[("community-asr", "1.0")]),
        inventory_complete: true,
        backends: vec![BackendDiagnostic {
            snap_name: "community-asr".into(),
            ..BackendDiagnostic::default()
        }],
        ..DiagnosticInput::default()
    });
    assert_eq!(missing_myna.onboarding(), OnboardingState::NoMyna);

    let incomplete = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(&[("myna", "1.2.3")]),
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
        installed_snaps: installed(&[("myna", "1.2.3"), ("myna-unrelated", "4.0")]),
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
}

#[test]
fn diagnostics_status_values_are_translatable_user_facing_labels() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(&[("myna", "1.2.3")]),
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
        "Dictation is not installed",
        "No model installed",
        "Ready",
        "Installation status unavailable",
        "Connected",
        "Multiple connections",
        "Not connected",
        "Machine",
        "Daemon",
        "Models",
        "Problems",
        "none selected",
    ] {
        assert!(pot.contains(&format!("msgid \"{label}\"")), "{label}");
    }
}

#[test]
fn failed_backend_discovery_does_not_claim_onboarding_is_complete() {
    let report = present_diagnostics(DiagnosticInput {
        installed_snaps: installed(&[("myna", "1.2.3")]),
        inventory_complete: true,
        problems: vec!["Model connections: discovery failed".into()],
        ..DiagnosticInput::default()
    });
    assert_eq!(report.onboarding(), OnboardingState::Unavailable);
}

mod performance_warnings {
    use super::has_field;
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
                ..Default::default()
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
            has_field(
                &text,
                "Clock",
                "cpu0 reached 0.86 GHz of 5.09 GHz, 8 cores in this class"
            ),
            "{text}"
        );
        assert!(has_field(&text, "Profile", "balanced"), "{text}");
        assert!(
            has_field(&text, "Power", "mains, battery not charging"),
            "{text}"
        );
        assert!(has_field(&text, "Pressure", "cpu 0.00%"), "{text}");
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
            warning
                .remedy
                .contains("A power tool has capped the CPU clock"),
            "{}",
            warning.remedy
        );
        assert!(!warning.remedy.contains("scaling_max_freq"));
        assert!(
            report
                .copy_text()
                .contains("(policy allows 1.50 GHz, scaling_max_freq)"),
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
        let text = report.copy_text();
        assert!(
            has_field(
                &text,
                "Pressure",
                "cpu 0.00%, memory 42.10%, io 15.00% of the last 10 s stalled"
            ),
            "{text}"
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
            has_field(&unknown.copy_text(), "Clock", "(no cpufreq information)"),
            "{}",
            unknown.copy_text()
        );

        let unmeasured = present_diagnostics(ready_input(None));
        assert!(unmeasured.warnings().is_empty());
        assert!(
            has_field(&unmeasured.copy_text(), "Clock", "(not measured yet)"),
            "{}",
            unmeasured.copy_text()
        );
    }
}
