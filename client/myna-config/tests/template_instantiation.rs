use std::path::{Path, PathBuf};
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
        "MynaPage",
        "BackendPage",
        "DiagnosticsPage",
        "StatusPage",
        "ShortcutDialog",
        "InstallModelsDialog",
        "OperationErrorDialog",
        "OnboardingWelcome",
        "OnboardingComponents",
        "OnboardingShortcut",
        "OnboardingWindow",
    ] {
        assert!(stdout.contains(name), "{name} was not instantiated");
    }
    assert!(
        stdout.contains("onboarding-shortcut: no room held for absent key caps"),
        "the shortcut step holds room for key caps it does not show"
    );
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
    assert!(stdout.contains("main-menu: setup and about"));
    assert!(stdout.contains("close-accelerator: bound"));
    assert!(stdout.contains("quit-accelerator: closes windows"));
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

/// A scratch config home and schema dir for a probe that opens the settings
/// store. The probe reads the default schema source, and a build machine has no
/// com.canonical.Myna.Dictation installed: compile the crate's own copy there.
fn scratch_store(tag: &str) -> (PathBuf, PathBuf) {
    let store = std::env::temp_dir().join(format!("myna-config-{tag}-{}", std::process::id()));
    let schemas = store.join("schemas");
    std::fs::create_dir_all(&schemas).expect("create scratch schema dir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml"),
        schemas.join("com.canonical.Myna.Dictation.gschema.xml"),
    )
    .expect("stage schema");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media-keys.gschema.xml"),
        schemas.join("media-keys.gschema.xml"),
    )
    .expect("stage media-keys schema");
    assert!(Command::new("glib-compile-schemas")
        .arg(&schemas)
        .status()
        .expect("glib-compile-schemas")
        .success());
    (store, schemas)
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

    let (store, schemas) = scratch_store("typing");
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
        // Never the live session's bus, where a real daemon would answer.
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/myna-config-probe",
        )
        .output()
        .expect("run onboarding probe");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "onboarding probe failed: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "onboarding-icon: themed",
        "onboarding-welcome: icon shown",
        "onboarding-chrome: welcome untitled, no back",
        "onboarding-wrap: welcome on one line each",
        "onboarding-start: advanced",
        "onboarding-layout: forward in view",
        "onboarding-chrome: components untitled, back",
        "onboarding-gate: held",
        "onboarding-wrap: components on one line each",
        "onboarding-commands: none",
        "onboarding-flag: a switch, off",
        "onboarding-rows: locked until the flag",
        "onboarding-refresh: re-read on focus",
        "onboarding-unreadable: said why",
        "onboarding-flag: a dismissed prompt reverts silently",
        "onboarding-flag: a refused prompt reverts with a toast",
        "onboarding-flag: a refusal's report names the snapd request",
        "onboarding-flag: a long report scrolls inside the window",
        "onboarding-flag: pending while snapd asks",
        "onboarding-flag: on, the list unlocked",
        "onboarding-flag: stays on",
        "onboarding-flag: a stale read does not undo it",
        "onboarding-install: a dismissed prompt reverts silently",
        "onboarding-install: one install at a time",
        "onboarding-install: the extension enables beside an install",
        "onboarding-install: the download's percentage shown",
        "onboarding-install: installed once snapd is done",
        "onboarding-install: a failed change reverts with a toast and its report",
        "onboarding-install: the model waits for its change",
        "onboarding-install: the model installed",
        "onboarding-install: an install started elsewhere is followed",
        "onboarding-extension: a failure reverts with a toast and its report",
        "onboarding-extension: enabling shown in the row",
        "onboarding-extension: enabled in the row, and Next moved on",
        "onboarding-rows: unlocked by the flag",
        "onboarding-poll: found without focus",
        "onboarding-status: the download shown",
        "onboarding-snapd: waits for the install to finish",
        "onboarding-auto: status before advancing",
        "onboarding-auto: set up once and advanced",
        "onboarding-poll: stopped once found",
        "onboarding-close: setup stopped with the wizard",
        "onboarding-auto: Next skips the pause",
        "onboarding-optional: the extension waits for Next",
        "onboarding-rows: an unavailable extension says it falls back",
        "onboarding-rows: an extension that cannot run says why",
        "onboarding-auto-failure: reported",
        "onboarding-auto-failure: the footer says so",
        "onboarding-auto-failure: Details name the cause",
        "onboarding-auto-failure: Next retries",
        "onboarding-setup-failure: reported",
        "onboarding-connect: a dismissed prompt stays silently",
        "onboarding-connect: a refusal is reported",
        "onboarding-connect: Next connects the model",
        "onboarding-installed: shown in the footer",
        "onboarding-rows: each installed",
        "onboarding-setup: back stays for a moment",
        "onboarding-setup: no spinner for a moment",
        "onboarding-setup: spinner while setting up",
        "onboarding-setup: no leaving past the moment",
        "onboarding-setup: back stays back",
        "onboarding-setup: restarted the daemon",
        "onboarding-walk: reached the last step",
        "onboarding-shortcut: headed as the design",
        "onboarding-wrap: shortcut title on one line",
        "onboarding-shortcut: waits for the daemon",
        "onboarding-shortcut: button plain",
        "onboarding-chrome: shortcut untitled, back",
        "onboarding-finish: Done closes Myna Settings",
    ] {
        assert!(stdout.contains(line), "onboarding probe missing: {line}");
    }
}

/// Against a running daemon the Myna page offers set-up for an unbound
/// shortcut, asks the daemon for its default, and renders the granted key.
#[test]
fn the_shortcut_row_binds_through_the_daemon_and_shows_the_key() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let (store, schemas) = scratch_store("shortcut");
    // A private bus: the probe serves its stand-in daemon there. No portals,
    // which that bus would otherwise start on GTK's behalf.
    let output = Command::new("dbus-run-session")
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        .env("GDK_DEBUG", "no-portals")
        .env("MYNA_CONFIG_SHORTCUT_TEST", "1")
        .output()
        .expect("run the shortcut probe under dbus-run-session");
    std::fs::remove_dir_all(&store).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "shortcut probe failed: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "shortcut-unbound: offered set-up",
        "shortcut-refused: error dialog",
        "shortcut-bound: Super+J",
    ] {
        assert!(stdout.contains(line), "shortcut probe missing: {line}");
    }
}

/// Where the portal has no GlobalShortcuts, set-up installs the desktop shortcut
/// itself rather than asking the daemon, and renders it.
#[test]
fn the_shortcut_row_installs_a_desktop_shortcut_under_control_activation() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let (store, schemas) = scratch_store("shortcut-control");
    let output = Command::new("dbus-run-session")
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        .env("GDK_DEBUG", "no-portals")
        .env("MYNA_CONFIG_SHORTCUT_CONTROL_TEST", "1")
        .env("GTK_A11Y", "none")
        .output()
        .expect("run the control shortcut probe under dbus-run-session");
    std::fs::remove_dir_all(&store).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "control shortcut probe failed: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "shortcut-unbound: offered set-up",
        "shortcut-bound: Super+J",
        "shortcut-dialog: example is the default key",
        "shortcut-dialog: example dimmed",
        "shortcut-changed: Ctrl+Alt+D",
        "shortcut-special-key: Calculator",
        "shortcut-reserved: Super+O refused",
        "shortcut-replaced: Super+L",
    ] {
        assert!(
            stdout.contains(line),
            "control shortcut probe missing: {line}"
        );
    }
    // The probe's own lines; the bus also starts portals, which warn too.
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("(process:") || line.starts_with("(myna-config:"))
        .filter(|line| line.contains("-WARNING **") || line.contains("-CRITICAL **"))
        .collect();
    assert!(warnings.is_empty(), "toolkit warnings: {warnings:#?}");
}

/// Setup installs the default key itself under control activation, but never
/// over a key the user has or another shortcut holds; under the portal it
/// raises the portal's own dialog, and a dismissal is not reported.
#[test]
fn onboarding_installs_the_default_key_only_under_control_activation() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let (store, schemas) = scratch_store("onboarding-control");
    let output = Command::new("dbus-run-session")
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        .env("GDK_DEBUG", "no-portals")
        .env("MYNA_CONFIG_ONBOARDING_CONTROL_TEST", "1")
        .env("GTK_A11Y", "none")
        .output()
        .expect("run the onboarding control probe under dbus-run-session");
    std::fs::remove_dir_all(&store).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "onboarding control probe failed: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "onboarding-default: kept the user's key",
        "onboarding-default: left a key in use",
        "onboarding-default: portal dialog raised on arrival",
        "onboarding-keys: set up leads while no key is bound",
        "onboarding-keys: Super+J under the portal",
        "onboarding-keys: Done leads once a key is bound",
        "onboarding-keys: follows a portal rebind",
        "onboarding-default: Super+J without a click",
        "onboarding-keys: Super+J under control",
        "onboarding-keys: follows a desktop rebind",
        "onboarding-restart: waits for the daemon's name",
    ] {
        assert!(
            stdout.contains(line),
            "onboarding control probe missing: {line}"
        );
    }
}

/// The real application against a fixture machine: still running once it has
/// started, and not a single GTK or libadwaita warning on the way.
#[test]
fn the_application_starts_without_toolkit_warnings() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let (store, schemas) = scratch_store("startup");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bin");
    let path = format!(
        "{}:{}",
        fixtures.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // `timeout` ends it: 124 means it was still up when time ran out.
    let output = Command::new("dbus-run-session")
        .args(["--", "timeout", "4"])
        .arg(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        .env("GDK_DEBUG", "no-portals")
        // The private bus has no accessibility bus to find.
        .env("GTK_A11Y", "none")
        .env("PATH", path)
        .output()
        .expect("run myna-config under dbus-run-session");
    std::fs::remove_dir_all(&store).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(124),
        "myna-config exited: {stderr}"
    );
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("(myna-config:"))
        .filter(|line| line.contains("-WARNING **") || line.contains("-CRITICAL **"))
        .collect();
    assert!(warnings.is_empty(), "toolkit warnings: {warnings:#?}");
}

/// The backend pages end to end through the real repository adapter, against a
/// fixture machine: discovery, a page's snapshot, and a change that applies
/// on its own and is read back, or is refused and put back.
#[test]
fn backend_pages_discover_and_apply_against_a_fixture_machine() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }

    let (store, schemas) = scratch_store("backends");
    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GSETTINGS_SCHEMA_DIR", &schemas)
        .env("XDG_CONFIG_HOME", &store)
        // Never the live session's bus, where a real daemon would answer.
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/myna-config-probe",
        )
        // The probe writes a choice; keep it out of the live snap store.
        .env("SNAP_USER_COMMON", &store)
        .env("MYNA_CONFIG_BACKENDS_TEST", "1")
        .output()
        .expect("run backends probe");
    std::fs::remove_dir_all(&store).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "backends probe failed: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "backends-discovered: 2",
        "general-order: ok",
        "model-group: lists the installed models",
        "model-group: a dismissed prompt reverts silently",
        "model-group: a pending switch spins on its target",
        "model-group: choosing a model switches to it",
        "model-group: a refused switch reverts with a toast",
        "mode: shows the active backend's default",
        "mode: a choice is stored as the user's",
        "sounds: the switch writes the setting",
        "backend-snapshot: read",
        "backend-apply: read back",
        "backend-apply: a refused change reverts with a toast",
        "diagnostics-report: lists backends",
        "refresh-accelerator: refreshes the tab",
        "setup: reopens the wizard",
        "diagnostics-onboarding: leads to setup",
    ] {
        assert!(stdout.contains(line), "backends probe missing: {line}");
    }
}
