//! `myna.config` as the snap ships it: the real wrapper script, the real
//! gsettings binary and the real compiled schema, run under the environment
//! snapcraft.yaml declares. Assertions are on the store the user is left with,
//! not on the argv the wrapper builds - argv can be right against a schema
//! that no longer exists.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A $SNAP tree holding everything `myna.config` reaches for at runtime: the
/// wrapper, gsettings, and the schema compiled the way the build compiles it.
struct TestSnap(PathBuf);

impl TestSnap {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/myna-config-snap-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
        let schemas = root.join("usr/share/glib-2.0/schemas");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        std::fs::create_dir_all(root.join("common/.config")).unwrap();
        std::fs::create_dir_all(root.join("shadow-path")).unwrap();
        std::fs::create_dir_all(&schemas).unwrap();

        std::fs::copy(
            repository_root().join("myna-snap/scripts/myna-config"),
            root.join("bin/myna-config"),
        )
        .unwrap();
        std::os::unix::fs::symlink(program_on_path("gsettings"), root.join("usr/bin/gsettings"))
            .unwrap();
        // The only gsettings on PATH refuses to run, so the wrapper has to
        // reach the one inside the snap by its $SNAP-relative path.
        let shadow = root.join("shadow-path/gsettings");
        std::fs::write(
            &shadow,
            "#!/bin/sh\necho 'shadowed gsettings' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy(schema_source(), schemas.join(SCHEMA_FILE)).unwrap();
        assert!(
            Command::new("glib-compile-schemas")
                .arg(&schemas)
                .status()
                .unwrap()
                .success(),
            "the shipped schema must compile"
        );

        Self(root)
    }

    /// Runs the wrapper with only what snapcraft.yaml declares - no host
    /// environment, so a schema or backend the snap does not carry cannot
    /// leak in from the machine running the test.
    fn run(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(self.0.join("bin/myna-config"));
        command
            .args(arguments)
            .env_clear()
            .env("SNAP", &self.0)
            .env("PATH", self.0.join("shadow-path"));
        for (key, value) in packaged_environment() {
            command.env(key, self.expand(&value));
        }
        command.output().unwrap()
    }

    /// snapd's variables, as snapd would expand them. $SNAP_USER_COMMON first:
    /// it has $SNAP as a prefix.
    fn expand(&self, value: &str) -> String {
        let root = self.0.to_string_lossy();
        value
            .replace("$SNAP_USER_COMMON", &format!("{root}/common"))
            .replace("$SNAP", &root)
    }

    /// The snap-private keyfile store, as a user would find it on disk, sorted
    /// because glib writes keys in the order they were set. Empty until
    /// something is written.
    fn store(&self) -> Vec<String> {
        let keyfile =
            std::fs::read_to_string(self.0.join("common/.config/glib-2.0/settings/keyfile"))
                .unwrap_or_default();
        let mut lines: Vec<_> = keyfile.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    }
}

impl Drop for TestSnap {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const SCHEMA_FILE: &str = "com.canonical.Myna.Dictation.gschema.xml";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema_source() -> PathBuf {
    repository_root()
        .join("client/data/glib-2.0/schemas")
        .join(SCHEMA_FILE)
}

fn snapcraft_yaml() -> String {
    std::fs::read_to_string(repository_root().join("myna-snap/snap/snapcraft.yaml")).unwrap()
}

fn program_on_path(program: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{program} must be installed to run the packaging tests"))
}

/// The `key: value` entries of one snapcraft.yaml mapping, given the line that
/// opens it and the indent of its entries. Comments are skipped; the block
/// ends at the first line indented less than its entries.
fn yaml_mapping(opening_line: &str, indent: &str) -> Vec<(String, String)> {
    let yaml = snapcraft_yaml();
    let body = yaml
        .split_once(opening_line)
        .unwrap_or_else(|| panic!("snapcraft.yaml no longer has `{}`", opening_line.trim()))
        .1;
    let entries: Vec<_> = body
        .lines()
        .take_while(|line| line.starts_with(indent))
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| {
            let (key, value) = line
                .trim()
                .split_once(": ")
                .expect("entries are plain `key: value` pairs");
            (key.to_owned(), value.to_owned())
        })
        .collect();
    assert!(!entries.is_empty(), "`{}` is empty", opening_line.trim());
    entries
}

/// What snapd sets for `myna.config`: the snap-wide block plus the
/// `&settings-env` anchor the config app aliases.
fn packaged_environment() -> Vec<(String, String)> {
    let mut environment = yaml_mapping("\nenvironment:\n", "  ");
    environment.extend(packaged_settings_env());
    environment
}

fn packaged_settings_env() -> Vec<(String, String)> {
    yaml_mapping("      <<: &settings-env\n", "        ")
}

fn schema_keys() -> Vec<String> {
    std::fs::read_to_string(schema_source())
        .unwrap()
        .split("<key name=\"")
        .skip(1)
        .map(|tail| tail.split_once('"').unwrap().0.to_owned())
        .collect()
}

#[test]
fn bare_snap_config_lists_every_key_the_shipped_schema_declares() {
    let snap = TestSnap::new();

    let output = snap.run(&[]);

    assert!(output.status.success(), "{output:?}");
    let listing = String::from_utf8(output.stdout).unwrap();
    for key in schema_keys() {
        assert!(listing.contains(&key), "{key} missing from:\n{listing}");
    }
}

#[test]
fn snap_config_writes_through_to_the_snap_private_keyfile() {
    let snap = TestSnap::new();

    // Bare, unquoted, for a string-typed key: the wrapper leans on gsettings
    // falling back to string parsing when GVariant parsing fails.
    assert!(snap.run(&["set", "language", "en"]).status.success());
    assert!(snap.run(&["set", "hud-style", "ribbon"]).status.success());

    assert_eq!(
        snap.store(),
        [
            "[com/canonical/myna/dictation]",
            "hud-style='ribbon'",
            "language='en'",
        ]
    );
    assert_eq!(
        String::from_utf8(snap.run(&["get", "language"]).stdout).unwrap(),
        "'en'\n"
    );

    assert!(snap.run(&["reset", "language"]).status.success());
    assert_eq!(
        String::from_utf8(snap.run(&["get", "language"]).stdout).unwrap(),
        "''\n",
        "reset must fall back to the schema default"
    );
}

#[test]
fn snap_config_refuses_what_the_shipped_schema_refuses() {
    let snap = TestSnap::new();

    for arguments in [
        &["set", "hud-style", "vumetre"][..], // outside the enum
        &["set", "hud-styl", "ribbon"][..],   // no such key
    ] {
        let output = snap.run(arguments);
        assert!(!output.status.success(), "{arguments:?} should fail");
    }
    assert!(
        snap.store().is_empty(),
        "a rejected write must leave no store"
    );
}

#[test]
fn unlisted_verbs_pass_through_to_gsettings() {
    let snap = TestSnap::new();

    let output = snap.run(&["list-schemas"]);

    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("com.canonical.Myna.Dictation"));
}

/// The store the tests above write to is only the user's store if snapcraft
/// says so: keyfile, and COMMON rather than per-revision DATA that `snap
/// revert` would roll back.
#[test]
fn packaged_settings_env_pins_the_snap_private_keyfile_store() {
    let settings_env = packaged_settings_env();
    let settings_env: Vec<_> = settings_env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    assert_eq!(
        settings_env,
        [
            ("GSETTINGS_BACKEND", "keyfile"),
            ("XDG_CONFIG_HOME", "$SNAP_USER_COMMON/.config"),
        ]
    );
}

#[test]
fn snap_build_keeps_the_native_ui_host_only() {
    let yaml = snapcraft_yaml();

    assert!(!yaml.contains("cargo install -f --locked --path myna-config"));
    assert!(!yaml.contains("bin/myna-config-gtk"));
    assert!(yaml.contains("\"myna-config\": bin/myna-config"));
    let config_app = yaml
        .split_once("\n  config:\n")
        .unwrap()
        .1
        .split_once("\n  # The testbed CLI")
        .unwrap()
        .0;
    assert!(config_app.contains("environment: *settings-env"));
    assert!(!config_app.contains("extensions:"));
    assert!(!config_app.contains("plugs:"));
}
