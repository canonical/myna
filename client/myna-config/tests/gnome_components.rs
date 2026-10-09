//! The gnome-shell extension adapter against a stand-in shell on a private
//! peer-to-peer D-Bus connection: no bus daemon, no real shell.

use std::cell::RefCell;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;

use std::time::Duration;

use gio::glib::{self, MainContext, Variant, VariantDict};
use gio::prelude::*;
use myna_config::onboarding::SHELL_EXTENSION_UUID;
use myna_config::platform::gnome::components::GnomeComponents;
use myna_platform::components::{Blocker, ComponentError, ComponentStatus, Components, StepKind};

const SHELL_XML: &str = "<node>\
  <interface name='org.gnome.Shell.Extensions'>\
    <method name='GetExtensionInfo'>\
      <arg type='s' name='uuid' direction='in'/>\
      <arg type='a{sv}' name='info' direction='out'/>\
    </method>\
    <method name='EnableExtension'>\
      <arg type='s' name='uuid' direction='in'/>\
      <arg type='b' name='success' direction='out'/>\
    </method>\
    <property name='UserExtensionsEnabled' type='b' access='readwrite'/>\
  </interface>\
</node>";

fn socket(stream: UnixStream) -> gio::IOStream {
    let socket = gio::Socket::from_fd(stream.into()).expect("wrap the socket");
    socket.connection_factory_create_connection().upcast()
}

type Known = Rc<RefCell<Vec<(String, Variant)>>>;

/// The shell-wide switch, kept in `known` beside the extension's info.
const USER_EXTENSIONS_ENABLED: &str = "UserExtensionsEnabled";

/// What the stand-in shell does when asked to enable the extension.
#[derive(Clone, Copy)]
enum OnEnable {
    /// Starts it a moment later, as gnome-shell does once its setting
    /// changes.
    Start,
    /// Answers false, as gnome-shell does for a uuid it does not know.
    Refuse,
    /// Tries, and the extension errors.
    Fail,
    /// Reports it activating, and then it errors.
    ActivateThenFail,
    /// Accepts and never starts it.
    Ignore,
}

fn set(known: &Known, key: &str, value: Variant) {
    let mut known = known.borrow_mut();
    known.retain(|(existing, _)| existing != key);
    known.push((key.to_owned(), value));
}

/// A connected pair: the adapter's end and the stand-in shell's, which
/// answers `GetExtensionInfo` with whatever `known` holds for the uuid.
fn shell(known: Known) -> (gio::DBusConnection, gio::DBusConnection) {
    shell_enabling(known, OnEnable::Start)
}

fn shell_enabling(known: Known, on_enable: OnEnable) -> (gio::DBusConnection, gio::DBusConnection) {
    let (client, server) = UnixStream::pair().expect("socket pair");
    let guid = gio::dbus_generate_guid();
    let (client, server) = MainContext::ref_thread_default().block_on(async {
        futures_join(
            gio::DBusConnection::new_future(
                &socket(client),
                None,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT,
                None,
            ),
            gio::DBusConnection::new_future(
                &socket(server),
                Some(&guid),
                gio::DBusConnectionFlags::AUTHENTICATION_SERVER
                    | gio::DBusConnectionFlags::AUTHENTICATION_ALLOW_ANONYMOUS,
                None,
            ),
        )
        .await
    });
    let (client, server) = (client.expect("client end"), server.expect("server end"));
    let interface = gio::DBusNodeInfo::for_xml(SHELL_XML)
        .unwrap()
        .lookup_interface("org.gnome.Shell.Extensions")
        .unwrap();
    let properties = known.clone();
    server
        .register_object("/org/gnome/Shell", &interface)
        .method_call(move |_, _, _, _, method, parameters, invocation| {
            let (uuid,) = parameters.get::<(String,)>().unwrap();
            if method == "EnableExtension" {
                let known = known.clone();
                let now = known.clone();
                let later = move |state: f64, error: &'static str| {
                    MainContext::ref_thread_default().spawn_local(async move {
                        glib::timeout_future(Duration::from_millis(50)).await;
                        set(&known, "state", state.to_variant());
                        set(&known, "error", error.to_variant());
                    });
                };
                let accepted = match on_enable {
                    OnEnable::Start => {
                        later(1.0, "");
                        true
                    }
                    OnEnable::ActivateThenFail => {
                        set(&now, "state", 8.0.to_variant());
                        later(3.0, "Error: gone mid-start");
                        true
                    }
                    OnEnable::Fail => {
                        later(3.0, "TypeError: boom");
                        true
                    }
                    OnEnable::Refuse => false,
                    OnEnable::Ignore => true,
                };
                invocation.return_value(Some(&(accepted,).to_variant()));
                return;
            }
            let dict = VariantDict::new(None);
            for (key, value) in known.borrow().iter() {
                if uuid == SHELL_EXTENSION_UUID && key != USER_EXTENSIONS_ENABLED {
                    dict.insert_value(key, value);
                }
            }
            invocation.return_value(Some(&Variant::tuple_from_iter([dict.end()])));
        })
        .property(move |_, _, _, _, _| {
            properties
                .borrow()
                .iter()
                .find(|(key, _)| key == USER_EXTENSIONS_ENABLED)
                .map_or_else(|| true.to_variant(), |(_, value)| value.clone())
        })
        .build()
        .expect("register the stand-in shell");
    (client, server)
}

/// Both handshakes must progress together: each side waits on the other.
async fn futures_join<A, B: 'static>(
    a: impl std::future::Future<Output = A>,
    b: impl std::future::Future<Output = B> + 'static,
) -> (A, B) {
    let b = MainContext::ref_thread_default().spawn_local(b);
    let a = a.await;
    (a, b.await.expect("server handshake"))
}

fn install_copy(data_dir: &std::path::Path) {
    let extension = data_dir
        .join("gnome-shell/extensions")
        .join(SHELL_EXTENSION_UUID);
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(extension.join("metadata.json"), "{}").unwrap();
}

/// The system data dirs and the user's, holding the copies asked for.
struct DataDirs {
    _system: tempdir::Dir,
    _user: tempdir::Dir,
    system_dirs: Vec<PathBuf>,
    user_dir: PathBuf,
}

fn data_dirs(with_system_copy: bool, with_user_copy: bool) -> DataDirs {
    let (system, user) = (tempdir::Dir::new(), tempdir::Dir::new());
    if with_system_copy {
        install_copy(system.path());
    }
    if with_user_copy {
        install_copy(user.path());
    }
    DataDirs {
        system_dirs: vec![system.path().to_owned()],
        user_dir: user.path().to_owned(),
        _system: system,
        _user: user,
    }
}

mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "myna-shell-extensions-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Everything on one context: the stand-in shell answers on the context it
/// was registered from, and the test blocks on that one.
fn on_own_context<T>(test: impl FnOnce() -> T) -> T {
    MainContext::new()
        .with_thread_default(test)
        .expect("own the test's context")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    MainContext::ref_thread_default().block_on(future)
}

fn state(reported: &[(&str, f64)], with_system_copy: bool) -> ComponentStatus {
    state_with(reported, with_system_copy, false)
}

fn state_with(
    reported: &[(&str, f64)],
    with_system_copy: bool,
    with_user_copy: bool,
) -> ComponentStatus {
    on_own_context(|| {
        let known = Rc::new(RefCell::new(
            reported
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_variant()))
                .collect(),
        ));
        let dirs = data_dirs(with_system_copy, with_user_copy);
        let (client, _server) = shell(known);
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        );
        block_on(extensions.status(SHELL_EXTENSION_UUID))
    })
}

#[test]
fn an_enabled_system_copy_is_enabled() {
    assert_eq!(
        state(&[("type", 1.0), ("state", 1.0)], true),
        ComponentStatus::Active
    );
}

#[test]
fn a_disabled_system_copy_can_be_enabled() {
    assert_eq!(
        state(&[("type", 1.0), ("state", 2.0)], true),
        ComponentStatus::Inactive
    );
}

#[test]
fn a_user_copy_is_not_the_packaged_extension() {
    assert_eq!(
        state_with(&[("type", 2.0), ("state", 1.0)], false, true),
        ComponentStatus::Unavailable
    );
}

#[test]
fn a_user_copy_on_disk_shadows_a_system_copy() {
    assert_eq!(
        state_with(&[("type", 2.0), ("state", 1.0)], true, true),
        ComponentStatus::Blocked(Blocker::Shadowed)
    );
    assert_eq!(
        state_with(&[], true, true),
        ComponentStatus::Blocked(Blocker::Shadowed)
    );
}

#[test]
fn a_system_copy_the_shell_cannot_run_says_why() {
    assert_eq!(
        state(&[("type", 1.0), ("state", 3.0)], true),
        ComponentStatus::Failed
    );
    assert_eq!(
        state(&[("type", 1.0), ("state", 4.0)], true),
        ComponentStatus::Blocked(Blocker::Incompatible)
    );
}

/// gnome-shell's settings, compiled from the fixture into a directory of
/// their own, on a backend that keeps them in memory or, `read_only`, lets
/// nothing be written.
struct ShellSettings {
    _dir: tempdir::Dir,
    settings: gio::Settings,
}

fn shell_settings(read_only: bool) -> ShellSettings {
    let dir = tempdir::Dir::new();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gnome-shell.gschema.xml"),
        dir.path().join("gnome-shell.gschema.xml"),
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    let source = gio::SettingsSchemaSource::from_directory(dir.path(), None, false).unwrap();
    let backend = if read_only {
        gio::functions::null_settings_backend_new()
    } else {
        gio::functions::memory_settings_backend_new()
    };
    let settings = gio::Settings::new_full(
        &source.lookup("org.gnome.shell", false).unwrap(),
        Some(&backend),
        None,
    );
    ShellSettings {
        _dir: dir,
        settings,
    }
}

/// The state of a system copy gnome-shell has not scanned, with these
/// settings, then the outcome of enabling it and the state after. The
/// stand-in shell refuses `EnableExtension`, as gnome-shell does for a uuid
/// it has not scanned.
fn unscanned(
    settings: Option<&gio::Settings>,
) -> (ComponentStatus, Result<(), ComponentError>, ComponentStatus) {
    on_own_context(|| {
        let dirs = data_dirs(true, false);
        let (client, _server) = shell_enabling(Rc::default(), OnEnable::Refuse);
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        )
        .with_shell_settings(settings.cloned());
        let before = block_on(extensions.status(SHELL_EXTENSION_UUID));
        let outcome = block_on(extensions.enable(SHELL_EXTENSION_UUID));
        let after = block_on(extensions.status(SHELL_EXTENSION_UUID));
        (before, outcome, after)
    })
}

fn strv(settings: &gio::Settings, key: &str) -> Vec<String> {
    settings.strv(key).iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_system_copy_the_shell_does_not_list_needs_a_relogin() {
    let shell = shell_settings(false);
    shell
        .settings
        .set_strv("enabled-extensions", ["ubuntu-dock@ubuntu.com"])
        .unwrap();
    let (before, outcome, after) = unscanned(Some(&shell.settings));
    assert_eq!(before, ComponentStatus::NeedsRelogin);
    assert_eq!(outcome, Ok(()));
    assert_eq!(after, ComponentStatus::ActiveAfterRelogin);
    assert_eq!(
        strv(&shell.settings, "enabled-extensions"),
        ["ubuntu-dock@ubuntu.com", SHELL_EXTENSION_UUID]
    );
}

#[test]
fn enabling_an_unscanned_copy_lifts_a_disable_as_the_shell_would() {
    let shell = shell_settings(false);
    shell
        .settings
        .set_strv(
            "disabled-extensions",
            [SHELL_EXTENSION_UUID, "ding@rastersoft.com"],
        )
        .unwrap();
    shell
        .settings
        .set_strv("enabled-extensions", [SHELL_EXTENSION_UUID])
        .unwrap();
    // Listed in both: gnome-shell does not start a disabled extension.
    let (before, outcome, after) = unscanned(Some(&shell.settings));
    assert_eq!(before, ComponentStatus::NeedsRelogin);
    assert_eq!(outcome, Ok(()));
    assert_eq!(after, ComponentStatus::ActiveAfterRelogin);
    assert_eq!(
        strv(&shell.settings, "enabled-extensions"),
        [SHELL_EXTENSION_UUID]
    );
    assert_eq!(
        strv(&shell.settings, "disabled-extensions"),
        ["ding@rastersoft.com"]
    );
}

#[test]
fn an_unscanned_copy_already_listed_waits_for_the_login() {
    let shell = shell_settings(false);
    shell
        .settings
        .set_strv("enabled-extensions", [SHELL_EXTENSION_UUID])
        .unwrap();
    assert_eq!(
        unscanned(Some(&shell.settings)).0,
        ComponentStatus::ActiveAfterRelogin
    );
}

#[test]
fn an_unscanned_copy_under_a_lockdown_is_locked() {
    let shell = shell_settings(true);
    let (before, outcome, _) = unscanned(Some(&shell.settings));
    assert_eq!(before, ComponentStatus::Blocked(Blocker::Locked));
    match outcome {
        Err(ComponentError::Failed {
            kind: StepKind::Setting,
            step,
            ..
        }) => assert_eq!(step, "org.gnome.shell enabled-extensions"),
        other => panic!("expected a failed setting, got {other:?}"),
    }
}

#[test]
fn an_unscanned_copy_with_extensions_off_is_turned_off() {
    let shell = shell_settings(false);
    shell
        .settings
        .set_boolean("disable-user-extensions", true)
        .unwrap();
    assert_eq!(
        unscanned(Some(&shell.settings)).0,
        ComponentStatus::Blocked(Blocker::TurnedOff)
    );
}

#[test]
fn an_unscanned_copy_with_no_shell_settings_is_unavailable() {
    assert_eq!(unscanned(None).0, ComponentStatus::Unavailable);
}

#[test]
fn an_extension_neither_listed_nor_installed_is_unavailable() {
    assert_eq!(state(&[], false), ComponentStatus::Unavailable);
}

#[test]
fn a_shell_that_is_gone_is_no_shell() {
    let state = on_own_context(|| {
        let dirs = data_dirs(false, false);
        let (client, server) = shell(Rc::default());
        block_on(server.close_future()).unwrap();
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        );
        block_on(extensions.status(SHELL_EXTENSION_UUID))
    });
    assert_eq!(state, ComponentStatus::Unavailable);
}

/// Enable the extension through a stand-in shell that reports it disabled,
/// then read it back.
fn enable(on_enable: OnEnable, settle: Duration) -> (Result<(), ComponentError>, ComponentStatus) {
    on_own_context(|| {
        let known: Known = Rc::default();
        set(&known, "type", 1.0.to_variant());
        set(&known, "state", 2.0.to_variant());
        let dirs = data_dirs(true, false);
        let (client, _server) = shell_enabling(known, on_enable);
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        )
        .with_settle_timeout(settle);
        let outcome = block_on(extensions.enable(SHELL_EXTENSION_UUID));
        (outcome, block_on(extensions.status(SHELL_EXTENSION_UUID)))
    })
}

const ENABLE_CALL: &str =
    "org.gnome.Shell.Extensions.EnableExtension(\"myna-shell@canonical.com\")";

fn failure(outcome: Result<(), ComponentError>) -> (String, String) {
    match outcome {
        Err(ComponentError::Failed {
            kind: StepKind::Call,
            step: call,
            message,
        }) => (call, message),
        other => panic!("expected a failed D-Bus call, got {other:?}"),
    }
}

#[test]
fn enabling_waits_until_the_shell_runs_it() {
    let (outcome, state) = enable(OnEnable::Start, Duration::from_secs(5));
    assert_eq!(outcome, Ok(()));
    assert_eq!(state, ComponentStatus::Active);
}

#[test]
fn a_refused_enable_names_the_call() {
    let (call, message) = failure(enable(OnEnable::Refuse, Duration::from_secs(5)).0);
    assert_eq!(call, ENABLE_CALL);
    assert!(message.contains("does not know"), "{message}");
}

#[test]
fn an_extension_that_errors_reports_the_shells_error() {
    let (call, message) = failure(enable(OnEnable::Fail, Duration::from_secs(5)).0);
    assert_eq!(call, ENABLE_CALL);
    assert!(message.contains("TypeError: boom"), "{message}");
}

#[test]
fn an_extension_that_never_starts_gives_up() {
    let (call, message) = failure(enable(OnEnable::Ignore, Duration::from_millis(300)).0);
    assert_eq!(call, ENABLE_CALL);
    assert!(message.contains("did not start"), "{message}");
}

#[test]
fn enabling_with_no_shell_fails_with_the_bus_error() {
    let outcome = on_own_context(|| {
        let dirs = data_dirs(true, false);
        let (client, server) = shell(Rc::default());
        block_on(server.close_future()).unwrap();
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        );
        block_on(extensions.enable(SHELL_EXTENSION_UUID))
    });
    let (call, message) = failure(outcome);
    assert_eq!(call, ENABLE_CALL);
    assert!(!message.is_empty());
}

#[test]
fn a_locked_extension_cannot_be_enabled() {
    assert_eq!(
        state(&[("type", 1.0), ("state", 2.0)], true),
        ComponentStatus::Inactive
    );
    let locked = on_own_context(|| {
        let known: Known = Rc::default();
        set(&known, "type", 1.0.to_variant());
        set(&known, "state", 2.0.to_variant());
        set(&known, "canChange", false.to_variant());
        let dirs = data_dirs(true, false);
        let (client, _server) = shell(known);
        let extensions = GnomeComponents::with_connection(
            client,
            dirs.system_dirs.clone(),
            dirs.user_dir.clone(),
        );
        block_on(extensions.status(SHELL_EXTENSION_UUID))
    });
    assert_eq!(locked, ComponentStatus::Blocked(Blocker::Locked));
}

#[test]
fn an_extension_held_off_by_the_extensions_switch_is_turned_off() {
    let held = |user_extensions: bool| {
        on_own_context(|| {
            let known: Known = Rc::default();
            set(&known, "type", 1.0.to_variant());
            set(&known, "state", 6.0.to_variant());
            set(&known, "canChange", false.to_variant());
            set(
                &known,
                USER_EXTENSIONS_ENABLED,
                user_extensions.to_variant(),
            );
            let dirs = data_dirs(true, false);
            let (client, _server) = shell(known);
            let extensions = GnomeComponents::with_connection(
                client,
                dirs.system_dirs.clone(),
                dirs.user_dir.clone(),
            );
            block_on(extensions.status(SHELL_EXTENSION_UUID))
        })
    };
    assert_eq!(held(false), ComponentStatus::Blocked(Blocker::TurnedOff));
    // With the switch on, only the administrator's lockdown is left.
    assert_eq!(held(true), ComponentStatus::Blocked(Blocker::Locked));
}

#[test]
fn an_extension_that_fails_while_activating_is_not_enabled() {
    let (call, message) = failure(enable(OnEnable::ActivateThenFail, Duration::from_secs(5)).0);
    assert_eq!(call, ENABLE_CALL);
    assert!(message.contains("gone mid-start"), "{message}");
}
