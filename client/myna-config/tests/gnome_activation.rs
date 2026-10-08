use std::path::{Path, PathBuf};

use gio::prelude::*;
use myna_config::platform::gnome::activation::GnomeActivation;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError, Conflict};

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const OURS: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/";
const THEIRS: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";

struct Schemas(PathBuf);

impl Drop for Schemas {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn schemas(tag: &str) -> (Schemas, gio::SettingsSchemaSource) {
    let dir = std::env::temp_dir().join(format!(
        "myna-desktop-shortcut-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media-keys.gschema.xml"),
        dir.join("media-keys.gschema.xml"),
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(&dir)
        .status()
        .unwrap()
        .success());
    let source = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
    (Schemas(dir), source)
}

fn key(text: &str) -> Accelerator {
    Accelerator::parse(text).unwrap()
}

fn bind(shortcut: &GnomeActivation, name: &str, command: &str, binding: &str) {
    let action = Action {
        name: name.to_owned(),
        command: command.to_owned(),
    };
    shortcut.bind(&key(binding), &action).unwrap();
}

fn binding(shortcut: &GnomeActivation) -> Option<String> {
    shortcut.binding().unwrap().map(|key| key.to_string())
}

fn conflict(shortcut: &GnomeActivation, binding: &str) -> Option<Conflict> {
    shortcut
        .conflicts(&key(binding))
        .unwrap()
        .into_iter()
        .next()
}

fn list(source: &gio::SettingsSchemaSource, backend: &gio::SettingsBackend) -> gio::Settings {
    gio::Settings::new_full(
        &source.lookup(MEDIA_KEYS, false).unwrap(),
        Some(backend),
        None,
    )
}

#[test]
fn nothing_installed_reads_as_no_binding() {
    let (_dir, source) = schemas("empty");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    assert_eq!(binding(&shortcut), None);
}

#[test]
fn install_binds_the_toggle_and_keeps_other_shortcuts() {
    let (_dir, source) = schemas("install");
    let backend = gio::functions::memory_settings_backend_new();
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();

    bind(&shortcut, "Dictation", "/snap/bin/myna.toggle", "<Super>j");
    bind(&shortcut, "Dictation", "/snap/bin/myna.toggle", "<Super>j");

    assert_eq!(binding(&shortcut).as_deref(), Some("<Super>j"));
    let paths: Vec<String> = list(&source, &backend)
        .strv("custom-keybindings")
        .iter()
        .map(|path| path.to_string())
        .collect();
    assert_eq!(paths, [THEIRS, OURS]);
    let entry = gio::Settings::new_full(
        &source
            .lookup(&format!("{MEDIA_KEYS}.custom-keybinding"), false)
            .unwrap(),
        Some(&backend),
        Some(OURS),
    );
    assert_eq!(entry.string("command"), "/snap/bin/myna.toggle");
    assert_eq!(
        shortcut.command().unwrap().as_deref(),
        Some("/snap/bin/myna.toggle")
    );
    assert_eq!(entry.string("name"), "Dictation");
}

#[test]
fn an_entry_left_out_of_the_list_is_not_a_binding() {
    let (_dir, source) = schemas("unlisted");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    bind(&shortcut, "Dictation", "/snap/bin/myna.toggle", "<Super>j");
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    assert_eq!(binding(&shortcut), None);
}

#[test]
fn a_desktop_without_the_schema_has_no_shortcut_to_manage() {
    let dir =
        std::env::temp_dir().join(format!("myna-desktop-shortcut-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = Schemas(dir.clone());
    std::fs::write(
        dir.join("other.gschema.xml"),
        r#"<schemalist><schema id="org.example.Other"/></schemalist>"#,
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(&dir)
        .status()
        .unwrap()
        .success());
    let source = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
    assert!(GnomeActivation::open_with(&source, None).is_none());
}

#[test]
fn a_key_the_desktop_uses_is_a_conflict_named_by_its_action() {
    let (_dir, source) = schemas("conflict");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    assert_eq!(
        conflict(&shortcut, "<Mod4>L").map(|conflict| conflict.action),
        Some("Lock screen".to_owned())
    );
    assert_eq!(
        conflict(&shortcut, "<Control><Alt>w").map(|conflict| conflict.action),
        Some("Close window".to_owned())
    );
    assert!(conflict(&shortcut, "<Super>j").is_none());
}

#[test]
fn another_custom_shortcut_is_a_conflict_but_ours_is_not() {
    let (_dir, source) = schemas("custom-conflict");
    let backend = gio::functions::memory_settings_backend_new();
    let theirs = gio::Settings::new_full(
        &source
            .lookup(&format!("{MEDIA_KEYS}.custom-keybinding"), false)
            .unwrap(),
        Some(&backend),
        Some(THEIRS),
    );
    theirs.set_string("name", "Terminal").unwrap();
    theirs.set_string("binding", "<Super>t").unwrap();
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    bind(&shortcut, "Dictation", "/snap/bin/myna.toggle", "<Super>j");

    assert_eq!(
        conflict(&shortcut, "<Super>t").map(|conflict| conflict.action),
        Some("Terminal".to_owned())
    );
    assert!(conflict(&shortcut, "<Super>j").is_none());
}

#[test]
fn releasing_a_conflict_removes_only_that_key() {
    let (_dir, source) = schemas("release");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    let held = conflict(&shortcut, "<Control><Alt>w").unwrap();

    shortcut.release(&held).unwrap();

    let wm = gio::Settings::new_full(
        &source
            .lookup("org.gnome.desktop.wm.keybindings", false)
            .unwrap(),
        Some(&backend),
        None,
    );
    let close: Vec<String> = wm.strv("close").iter().map(|a| a.to_string()).collect();
    assert_eq!(close, ["<Alt>F4"]);
    assert!(conflict(&shortcut, "<Control><Alt>w").is_none());
}

#[test]
fn a_static_key_is_reserved_and_an_editable_one_is_not() {
    let (_dir, source) = schemas("reserved");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    let rotate = conflict(&shortcut, "<Super>o").unwrap();
    assert_eq!(rotate.action, "Toggle automatic screen orientation");
    assert!(rotate.reserved);
    assert!(!conflict(&shortcut, "<Super>l").unwrap().reserved);
}

#[test]
fn a_reserved_key_is_not_released() {
    let (_dir, source) = schemas("reserved-release");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    let rotate = conflict(&shortcut, "<Super>o").unwrap();
    assert!(matches!(
        shortcut.release(&rotate),
        Err(ActivationError::Reserved(_))
    ));
    assert!(conflict(&shortcut, "<Super>o").is_some());
}

#[test]
fn clearing_unlists_the_entry_and_keeps_other_shortcuts() {
    let (_dir, source) = schemas("clear");
    let backend = gio::functions::memory_settings_backend_new();
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    let shortcut = GnomeActivation::open_with(&source, Some(&backend)).unwrap();
    bind(&shortcut, "Dictation", "/snap/bin/myna.toggle", "<Super>j");
    shortcut.clear().unwrap();
    let paths: Vec<String> = list(&source, &backend)
        .strv("custom-keybindings")
        .iter()
        .map(|path| path.to_string())
        .collect();
    assert_eq!(paths, [THEIRS]);
    assert_eq!(binding(&shortcut), None);
    assert_eq!(shortcut.command().unwrap(), None);
}

mod conformance {
    use super::*;
    use myna_platform::conformance::activation::{run, Fixture};

    struct Gnome {
        _dir: Schemas,
        source: gio::SettingsSchemaSource,
        backend: gio::SettingsBackend,
        holds: usize,
    }

    impl Gnome {
        fn entry(&self, path: &str) -> gio::Settings {
            gio::Settings::new_full(
                &self
                    .source
                    .lookup(&format!("{MEDIA_KEYS}.custom-keybinding"), false)
                    .unwrap(),
                Some(&self.backend),
                Some(path),
            )
        }
    }

    impl Fixture for Gnome {
        fn setup(&mut self) -> Box<dyn Activation> {
            self.backend = gio::functions::memory_settings_backend_new();
            self.holds = 0;
            Box::new(GnomeActivation::open_with(&self.source, Some(&self.backend)).unwrap())
        }

        fn hold(&mut self, accelerator: &Accelerator, reserved: bool) -> bool {
            if reserved {
                list(&self.source, &self.backend)
                    .set_strv("rotate-video-lock-static", [accelerator.as_str()])
                    .unwrap();
                return true;
            }
            let path = format!(
                "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/held{}/",
                self.holds
            );
            self.holds += 1;
            let entry = self.entry(&path);
            entry.set_string("name", "Held").unwrap();
            entry.set_string("binding", accelerator.as_str()).unwrap();
            let settings = list(&self.source, &self.backend);
            let mut paths: Vec<String> = settings
                .strv("custom-keybindings")
                .iter()
                .map(|path| path.to_string())
                .collect();
            paths.push(path);
            settings.set_strv("custom-keybindings", paths).unwrap();
            true
        }

        fn commands(&self) -> [&'static str; 2] {
            ["/snap/bin/myna.toggle", "gdbus call"]
        }

        fn change_outside(&mut self, binding: Option<&Accelerator>) {
            let entry = self.entry(OURS);
            entry
                .set_string("binding", binding.map_or("", Accelerator::as_str))
                .unwrap();
            let settings = list(&self.source, &self.backend);
            let mut paths: Vec<String> = settings
                .strv("custom-keybindings")
                .iter()
                .map(|path| path.to_string())
                .filter(|path| path != OURS)
                .collect();
            if binding.is_some() {
                paths.push(OURS.to_owned());
            }
            settings.set_strv("custom-keybindings", paths).unwrap();
        }

        fn settle(&mut self) {
            let context = gio::glib::MainContext::default();
            while context.iteration(false) {}
        }
    }

    #[test]
    fn gnome_passes_the_activation_suite() {
        let (dir, source) = schemas("conformance");
        let report = run(&mut Gnome {
            _dir: dir,
            source,
            backend: gio::functions::memory_settings_backend_new(),
            holds: 0,
        });
        assert!(report.not_applicable.is_empty(), "{report:?}");
        assert_eq!(report.passed.len(), 9);
    }
}
