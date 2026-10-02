//! GNOME's store of portal shortcuts on a memory backend with the real
//! schema, and the portal backend's rebind call against a stand-in on a
//! peer-to-peer connection.

use std::cell::RefCell;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::glib::{MainContext, Variant};
use gio::prelude::*;
use myna_config::adapters::portal_shortcuts::{self, PortalShortcuts, Store, APP_ID, SHORTCUT_ID};

const SCHEMA: &str = "org.gnome.settings-daemon.global-shortcuts.application";
const DICTATE: &str = "[('dictate', {'shortcuts': <['<Super>j']>, 'description': <'Dictation (press to start and stop)'>})]";
const BOTH: &str = "[('other', {'shortcuts': <['<Alt>o']>, 'description': <'Other'>}), ('dictate', {'shortcuts': <['<Super>j']>, 'description': <'Dictation (press to start and stop)'>})]";

struct Schemas(PathBuf);

impl Drop for Schemas {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn schemas(tag: &str) -> (Schemas, gio::SettingsSchemaSource) {
    let dir = std::env::temp_dir().join(format!(
        "myna-portal-shortcuts-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/global-shortcuts.gschema.xml"),
        dir.join("global-shortcuts.gschema.xml"),
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

/// The app's entry as GNOME Settings sees it, on `backend`.
fn stored(source: &gio::SettingsSchemaSource, backend: &gio::SettingsBackend) -> gio::Settings {
    gio::Settings::new_full(
        &source.lookup(SCHEMA, false).unwrap(),
        Some(backend),
        Some(&format!(
            "/org/gnome/settings-daemon/global-shortcuts/{APP_ID}/"
        )),
    )
}

fn set(settings: &gio::Settings, text: &str) {
    settings
        .set_value("shortcuts", &Variant::parse(None, text).unwrap())
        .unwrap();
}

fn ids(settings: &gio::Settings) -> Vec<String> {
    settings
        .value("shortcuts")
        .iter()
        .map(|entry| entry.child_value(0).get::<String>().unwrap())
        .collect()
}

#[test]
fn no_schema_means_no_store() {
    let dir =
        std::env::temp_dir().join(format!("myna-portal-shortcuts-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = Schemas(dir.clone());
    // Another GNOME schema, but not gnome-settings-daemon's global shortcuts.
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
    assert!(PortalShortcuts::open_with(&source, None, APP_ID).is_none());
    assert!(Store::open_with(source, None).is_none());
}

#[test]
fn the_stored_key_is_read_per_shortcut() {
    let (_dir, source) = schemas("read");
    let backend = gio::functions::memory_settings_backend_new();
    set(&stored(&source, &backend), BOTH);
    let store = PortalShortcuts::open_with(&source, Some(&backend), APP_ID).unwrap();
    assert_eq!(store.accelerator(SHORTCUT_ID).as_deref(), Some("<Super>j"));
    assert_eq!(store.accelerator("other").as_deref(), Some("<Alt>o"));
    assert_eq!(store.accelerator("missing"), None);
}

#[test]
fn taking_removes_only_that_entry_and_putting_back_restores_it() {
    let (_dir, source) = schemas("take");
    let backend = gio::functions::memory_settings_backend_new();
    let gnome = stored(&source, &backend);
    set(&gnome, BOTH);
    let store = PortalShortcuts::open_with(&source, Some(&backend), APP_ID).unwrap();

    let taken = store.take(SHORTCUT_ID).expect("a stored key to take");
    assert_eq!(taken.accelerator, "<Super>j");
    assert_eq!(ids(&gnome), ["other"]);

    store.put_back(taken);
    assert_eq!(ids(&gnome), ["other", "dictate"]);
    assert_eq!(store.accelerator(SHORTCUT_ID).as_deref(), Some("<Super>j"));
}

#[test]
fn nothing_stored_is_nothing_to_take() {
    let (_dir, source) = schemas("empty");
    let backend = gio::functions::memory_settings_backend_new();
    let store = PortalShortcuts::open_with(&source, Some(&backend), APP_ID).unwrap();
    assert!(store.take(SHORTCUT_ID).is_none());
    set(
        &stored(&source, &backend),
        "[('dictate', {'shortcuts': <@as []>, 'description': <'Dictation'>})]",
    );
    assert!(
        store.take(SHORTCUT_ID).is_none(),
        "an entry with no key binds nothing"
    );
}

#[test]
fn a_key_stored_meanwhile_is_not_overwritten() {
    let (_dir, source) = schemas("meanwhile");
    let backend = gio::functions::memory_settings_backend_new();
    let gnome = stored(&source, &backend);
    set(&gnome, DICTATE);
    let store = PortalShortcuts::open_with(&source, Some(&backend), APP_ID).unwrap();
    let taken = store.take(SHORTCUT_ID).unwrap();
    // The dialog stored the user's new key before the answer arrived.
    set(
        &gnome,
        "[('dictate', {'shortcuts': <['<Super>k']>, 'description': <'Dictation (press to start and stop)'>})]",
    );
    store.put_back(taken);
    assert_eq!(store.accelerator(SHORTCUT_ID).as_deref(), Some("<Super>k"));
    assert_eq!(ids(&gnome), ["dictate"]);
}

const REBIND_XML: &str = "<node>\
  <interface name='org.gnome.GlobalShortcutsRebind'>\
    <method name='RebindShortcuts'>\
      <arg type='s' name='app_id' direction='in'/>\
      <arg type='a(sa{sv})' name='shortcuts' direction='in'/>\
    </method>\
  </interface>\
</node>";

fn socket(stream: UnixStream) -> gio::IOStream {
    let socket = gio::Socket::from_fd(stream.into()).expect("wrap the socket");
    socket.connection_factory_create_connection().upcast()
}

/// The adapter's end of a connection to a stand-in portal backend that
/// records each `RebindShortcuts` call.
fn portal(calls: Rc<RefCell<Vec<Variant>>>) -> (gio::DBusConnection, gio::DBusConnection) {
    let (client, server) = UnixStream::pair().expect("socket pair");
    let guid = gio::dbus_generate_guid();
    let context = MainContext::ref_thread_default();
    let (client, server) = context.block_on(async {
        let server =
            MainContext::ref_thread_default().spawn_local(gio::DBusConnection::new_future(
                &socket(server),
                Some(&guid),
                gio::DBusConnectionFlags::AUTHENTICATION_SERVER
                    | gio::DBusConnectionFlags::AUTHENTICATION_ALLOW_ANONYMOUS,
                None,
            ));
        let client = gio::DBusConnection::new_future(
            &socket(client),
            None,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT,
            None,
        )
        .await;
        (client, server.await.expect("server handshake"))
    });
    let (client, server) = (client.expect("client end"), server.expect("server end"));
    let interface = gio::DBusNodeInfo::for_xml(REBIND_XML)
        .unwrap()
        .lookup_interface("org.gnome.GlobalShortcutsRebind")
        .unwrap();
    server
        .register_object("/org/gnome/globalshortcuts", &interface)
        .method_call(move |_, _, _, _, _, parameters, invocation| {
            calls.borrow_mut().push(parameters);
            invocation.return_value(None);
        })
        .build()
        .expect("register the stand-in portal");
    (client, server)
}

#[test]
fn rebind_hands_the_portal_backend_the_stored_shortcuts() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let (client, _server) = portal(calls.clone());
    let shortcuts = Variant::parse(None, DICTATE).unwrap();
    MainContext::ref_thread_default()
        .block_on(portal_shortcuts::rebind(&client, APP_ID, shortcuts.clone()))
        .expect("the stand-in answers");
    let calls = calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].child_value(0).get::<String>().as_deref(),
        Some(APP_ID)
    );
    assert_eq!(calls[0].child_value(1), shortcuts);
}

#[test]
fn myna_is_looked_for_under_its_app_id_and_an_empty_one() {
    let (_dir, source) = schemas("apps");
    let backend = gio::functions::memory_settings_backend_new();
    let store = Store::open_with(source.clone(), Some(backend.clone())).unwrap();
    assert_eq!(store.myna_apps(), [APP_ID]);
    gio::Settings::new_full(
        &source
            .lookup("org.gnome.settings-daemon.global-shortcuts", false)
            .unwrap(),
        Some(&backend),
        None::<&str>,
    )
    .set_strv(
        "applications",
        [".", "org.gnome.Other", APP_ID, "myna_other"],
    )
    .unwrap();
    assert_eq!(store.myna_apps(), [APP_ID, ".", "myna_other"]);
    set(&stored(&source, &backend), DICTATE);
    assert_eq!(
        store.app(APP_ID).accelerator(SHORTCUT_ID).as_deref(),
        Some("<Super>j")
    );
}
