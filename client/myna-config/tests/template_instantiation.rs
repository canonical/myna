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
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/global-shortcuts.gschema.xml"),
        schemas.join("global-shortcuts.gschema.xml"),
    )
    .expect("stage global-shortcuts schema");
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

/// A session bus config that activates nothing, so a probe sees only the
/// names it owns, not the services installed on the machine.
fn bare_session_bus(dir: &Path) -> PathBuf {
    let services = dir.join("dbus-services");
    std::fs::create_dir_all(&services).expect("create the empty service dir");
    let config = dir.join("session.conf");
    std::fs::write(
        &config,
        format!(
            "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
             \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\
             <busconfig><type>session</type><keep_umask/>\
             <listen>unix:tmpdir=/tmp</listen>\
             <servicedir>{}</servicedir>\
             <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
             <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
            services.display()
        ),
    )
    .expect("write the session bus config");
    config
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
        "onboarding-wrap: components title on one line",
        "onboarding-commands: none",
        "onboarding-button: one Install all, sized",
        "onboarding-refresh: re-read on focus",
        "onboarding-unreadable: said why",
        "onboarding-install: a dismissed prompt stops silently",
        "onboarding-install: a refusal reverts with a toast and its report",
        "onboarding-install: a long report scrolls inside the window",
        "onboarding-install: each step named while it runs",
        "onboarding-install: the download's percentage shown",
        "onboarding-install: a failed step reverts with a toast and its report",
        "onboarding-install: the size covers only what is missing",
        "onboarding-install: the extension is enabled last",
        "onboarding-install: installed, then set up",
        "onboarding-install: the button keeps its place",
        "onboarding-install: Next skips the pause",
        "onboarding-install: an install started elsewhere is followed",
        "onboarding-install: a component waits for its change",
        "onboarding-install: followed to its end, the model left",
        "onboarding-extension: only it left, Next leads and a failure is reported",
        "onboarding-extension: enabled by the button, then moved on",
        "onboarding-partial: sized for the model alone",
        "onboarding-partial: a dismissed install prompt stops silently",
        "onboarding-partial: only the model installed, then moved on",
        "onboarding-optional: an unavailable extension holds nothing",
        "onboarding-poll: found without focus",
        "onboarding-status: the download shown",
        "onboarding-snapd: waits for the install to finish",
        "onboarding-auto: status before advancing",
        "onboarding-auto: set up once and advanced",
        "onboarding-poll: stopped once found",
        "onboarding-close: setup stopped with the wizard",
        "onboarding-auto: Next skips the pause",
        "onboarding-optional: found elsewhere, an unavailable extension holds nothing",
        "onboarding-auto-failure: reported",
        "onboarding-auto-failure: the step says so",
        "onboarding-auto-failure: Details name the cause",
        "onboarding-auto-failure: Next retries",
        "onboarding-setup-failure: reported",
        "onboarding-connect: a dismissed prompt stays silently",
        "onboarding-connect: a refusal is reported",
        "onboarding-connect: Next connects the model",
        "onboarding-installed: the button says so",
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
        "onboarding-finish: Done over Settings keeps Settings",
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
        "shortcut-refused: toast, report behind Details",
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
        .arg(format!(
            "--config-file={}",
            bare_session_bus(&store).display()
        ))
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
        "onboarding-modal: one parented dialog holds set up and Done",
        "onboarding-modal: another window's dialog holds the step without an error",
        "onboarding-modal: a dialog left open lets set up try again",
        "onboarding-modal: a cancelled dialog is no error",
        "onboarding-modal: a failed bind toasts with Details",
        "onboarding-modal: closing under the dialog releases it",
        "onboarding-keys: Super+J under the portal",
        "onboarding-keys: Done leads once a key is bound",
        "onboarding-change: the portal's dialog changes the key",
        "onboarding-change: a cancel keeps the old key",
        "onboarding-change: a failed change says so",
        "onboarding-change: with nothing stored the dialog still changes the key",
        "onboarding-change: a key under an empty app id is offered and kept",
        "onboarding-change: no provider falls back to GNOME Settings",
        "onboarding-keys: follows a portal rebind",
        "onboarding-default: Super+J without a click",
        "onboarding-keys: Super+J under control",
        "onboarding-keys: follows a desktop rebind",
        "onboarding-capture: Change waits in place, Done held",
        "onboarding-capture: Escape keeps the key",
        "onboarding-capture: reserved key refused in place",
        "onboarding-capture: new key taken",
        "onboarding-capture: a chosen key survives Back and Next",
        "onboarding-capture: swap asked, declining keeps waiting",
        "onboarding-capture: Set up captures F8 as a cap, leaving ends it",
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
