use std::path::{Path, PathBuf};

use gio::prelude::*;
use myna_config::adapters::desktop_shortcut::DesktopShortcut;

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
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    assert_eq!(shortcut.binding(), None);
}

#[test]
fn install_binds_the_toggle_and_keeps_other_shortcuts() {
    let (_dir, source) = schemas("install");
    let backend = gio::functions::memory_settings_backend_new();
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();

    shortcut
        .install("Dictation", "/snap/bin/myna.toggle", "<Super>j")
        .unwrap();
    shortcut
        .install("Dictation", "/snap/bin/myna.toggle", "<Super>j")
        .unwrap();

    assert_eq!(shortcut.binding().as_deref(), Some("<Super>j"));
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
    assert_eq!(shortcut.command(), "/snap/bin/myna.toggle");
    assert_eq!(entry.string("name"), "Dictation");
}

#[test]
fn an_entry_left_out_of_the_list_is_not_a_binding() {
    let (_dir, source) = schemas("unlisted");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    shortcut
        .install("Dictation", "/snap/bin/myna.toggle", "<Super>j")
        .unwrap();
    list(&source, &backend)
        .set_strv("custom-keybindings", [THEIRS])
        .unwrap();
    assert_eq!(shortcut.binding(), None);
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
    assert!(DesktopShortcut::open_with(&source, None).is_none());
}

#[test]
fn a_key_the_desktop_uses_is_a_conflict_named_by_its_action() {
    let (_dir, source) = schemas("conflict");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    assert_eq!(
        shortcut.conflict("<Mod4>L").map(|conflict| conflict.action),
        Some("Lock screen".to_owned())
    );
    assert_eq!(
        shortcut
            .conflict("<Control><Alt>w")
            .map(|conflict| conflict.action),
        Some("Close window".to_owned())
    );
    assert!(shortcut.conflict("<Super>j").is_none());
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
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    shortcut
        .install("Dictation", "/snap/bin/myna.toggle", "<Super>j")
        .unwrap();

    assert_eq!(
        shortcut
            .conflict("<Super>t")
            .map(|conflict| conflict.action),
        Some("Terminal".to_owned())
    );
    assert!(shortcut.conflict("<Super>j").is_none());
}

#[test]
fn releasing_a_conflict_removes_only_that_key() {
    let (_dir, source) = schemas("release");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    let conflict = shortcut.conflict("<Control><Alt>w").unwrap();

    shortcut.release(&conflict).unwrap();

    let wm = gio::Settings::new_full(
        &source
            .lookup("org.gnome.desktop.wm.keybindings", false)
            .unwrap(),
        Some(&backend),
        None,
    );
    let close: Vec<String> = wm.strv("close").iter().map(|a| a.to_string()).collect();
    assert_eq!(close, ["<Alt>F4"]);
    assert!(shortcut.conflict("<Control><Alt>w").is_none());
}

#[test]
fn a_static_key_is_reserved_and_an_editable_one_is_not() {
    let (_dir, source) = schemas("reserved");
    let backend = gio::functions::memory_settings_backend_new();
    let shortcut = DesktopShortcut::open_with(&source, Some(&backend)).unwrap();
    let rotate = shortcut.conflict("<Super>o").unwrap();
    assert_eq!(rotate.action, "Toggle automatic screen orientation");
    assert!(rotate.reserved);
    assert!(!shortcut.conflict("<Super>l").unwrap().reserved);
}
