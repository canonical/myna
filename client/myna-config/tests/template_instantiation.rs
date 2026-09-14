use std::path::Path;
use std::process::Command;

use myna_config::app::{appearance_policy, AppearancePolicy};

#[test]
fn every_top_level_template_instantiates_headlessly_when_enabled() {
    if std::env::var_os("MYNA_CONFIG_TEMPLATE_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_TEMPLATE_TESTS=1 under Xvfb");
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        .env("MYNA_CONFIG_TEMPLATE_TEST", "1")
        .output()
        .expect("run template-instantiation probe");
    assert!(
        output.status.success(),
        "template probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in [
        "MainWindow",
        "ActiveBackendDialog",
        "ApplyDialog",
        "BackendApplyControls",
        "MynaPage",
        "BackendPage",
        "DiagnosticsPage",
        "SidebarRow",
        "StatusPage",
        "OperationErrorDialog",
        "OnboardingWelcome",
        "OnboardingComponents",
        "OnboardingShortcut",
        "OnboardingWindow",
    ] {
        assert!(stdout.contains(name), "{name} was not instantiated");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for offender in ["Failed to parse markup", "unknown tag", "Pango-WARNING"] {
        assert!(
            !stderr.contains(offender),
            "template probe emitted markup warning: {offender}: {stderr}"
        );
    }
}

#[test]
fn application_exposes_accessible_diagnostics_controls_when_enabled() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("MYNA_CONFIG_ACCESSIBILITY_TEST", "1")
        .output()
        .expect("run accessibility probe");
    assert!(
        output.status.success(),
        "accessibility probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("template-metadata: verified"));
    assert!(stdout.contains("keyboard-traversal: verified"));
    assert!(stdout.contains("narrow-layout: collapsed"));
    assert!(stdout.contains("high-contrast: verified"));
    assert!(stdout.contains("reduced-motion: verified"));
    assert!(stdout.contains("appearance-policy: applied"));
}

#[test]
fn appearance_policy_tracks_system_contrast_and_motion_preferences() {
    assert_eq!(
        appearance_policy(false, true),
        AppearancePolicy {
            reduced_motion: true,
            high_contrast: true,
        }
    );
    assert_eq!(
        appearance_policy(true, false),
        AppearancePolicy {
            reduced_motion: false,
            high_contrast: false,
        }
    );
}

/// The regression: a row desensitized while its own write was in flight took
/// keyboard focus away from the entry the user was still typing in, and GTK
/// warned that its `GtkText` never received a focus-out.
#[test]
fn typing_into_a_text_row_keeps_focus_and_stays_editable_when_enabled() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let store = std::env::temp_dir().join(format!("myna-config-typing-{}", std::process::id()));
    // The probe opens the store through the default schema source, and a
    // build machine has no com.canonical.Myna.Dictation installed: compile the
    // crate's own copy into the scratch dir and add it to that source.
    let schemas = store.join("schemas");
    std::fs::create_dir_all(&schemas).expect("create scratch schema dir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml"),
        schemas.join("com.canonical.Myna.Dictation.gschema.xml"),
    )
    .expect("stage schema");
    assert!(Command::new("glib-compile-schemas")
        .arg(&schemas)
        .status()
        .expect("glib-compile-schemas")
        .success());
    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        // A scratch store, so the probe's write never touches the real one.
        .env("GSETTINGS_BACKEND", "keyfile")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        .env("MYNA_CONFIG_TYPING_TEST", "1")
        .output()
        .expect("run typing probe");
    std::fs::remove_dir_all(&store).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "typing probe failed: {stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("typing-focus: retained"));
    assert!(
        !stderr.contains("did not receive a focus-out event"),
        "typing probe emitted the GtkText focus-out warning: {stderr}"
    );
}

/// The wizard's buttons must actually drive it: the regression was a presented
/// window whose controller had already been dropped.
#[test]
fn the_onboarding_wizard_walks_when_its_buttons_are_activated() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("MYNA_CONFIG_ONBOARDING_TEST", "1")
        .output()
        .expect("run onboarding probe");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "onboarding probe failed: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "onboarding-start: advanced",
        "onboarding-gate: held",
        "onboarding-walk: reached the last step",
        "onboarding-finish: handed back",
    ] {
        assert!(stdout.contains(line), "onboarding probe missing: {line}");
    }
}
