//! The Xfce components against a scratch home, `/proc`, `PATH` and autostart
//! directories, and a scripted `im-config`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use myna_config::command::{CommandError, CommandOutput, FakeCommandRunner};
use myna_config::platform::xfce::components::{Host, XfceComponents, HUD_HOST_AUTOSTART, IBUS};
use myna_platform::components::{
    Blocker, ComponentError, ComponentStatus, Components, Purpose, StepKind,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("myna-xfce-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Session {
    scratch: Scratch,
    env: HashMap<String, String>,
}

impl Session {
    /// IBus installed, the session set up for it, its daemon running.
    fn ibus(tag: &str) -> Self {
        let session = Self {
            scratch: Scratch::new(tag),
            env: HashMap::from([
                ("GTK_IM_MODULE".to_owned(), "ibus".to_owned()),
                ("XMODIFIERS".to_owned(), "@im=ibus".to_owned()),
            ]),
        };
        session.install_ibus();
        session.run("ibus-daemon");
        session
    }

    fn install_ibus(&self) {
        std::fs::write(self.scratch.path("bin").join("ibus-daemon"), "").unwrap();
    }

    fn run(&self, comm: &str) {
        let process = self.scratch.path("proc").join(comm);
        std::fs::create_dir_all(&process).unwrap();
        std::fs::write(process.join("comm"), format!("{comm}\n")).unwrap();
    }

    fn without_session_env(mut self) -> Self {
        self.env.clear();
        self
    }

    fn xinputrc(&self, body: &str) {
        std::fs::write(self.scratch.path("home").join(".xinputrc"), body).unwrap();
    }

    fn autostart(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.scratch.path(name);
        let file = dir.join(HUD_HOST_AUTOSTART);
        std::fs::write(&file, body).unwrap();
        file
    }

    fn host(&self) -> Host {
        Host {
            env: self.env.clone(),
            path: vec![self.scratch.path("bin")],
            proc_dir: self.scratch.path("proc"),
            home: self.scratch.path("home"),
            system_autostart: vec![self.scratch.path("system")],
            user_autostart: self.scratch.path("user"),
        }
    }

    fn components(&self, runner: &FakeCommandRunner) -> XfceComponents {
        XfceComponents::with_host(self.host(), Arc::new(runner.clone()))
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    gio::glib::MainContext::new().block_on(future)
}

fn status(session: &Session, id: &str) -> ComponentStatus {
    block_on(session.components(&FakeCommandRunner::default()).status(id))
}

fn ok() -> Result<CommandOutput, CommandError> {
    Ok(CommandOutput::new(Some(0), "", ""))
}

fn refused(stderr: &str) -> Result<CommandOutput, CommandError> {
    Err(CommandError::NonZero {
        exit_status: Some(2),
        stdout: String::new(),
        stderr: stderr.to_owned(),
    })
}

fn args(runner: &FakeCommandRunner) -> Vec<Vec<String>> {
    runner
        .calls()
        .iter()
        .map(|call| {
            let mut words = vec![call.executable().to_owned()];
            words.extend(call.arguments().iter().cloned());
            words
        })
        .collect()
}

#[test]
fn xfce_needs_ibus_and_a_host_for_the_indicator() {
    let session = Session::ibus("required");
    let required = session.components(&FakeCommandRunner::default()).required();
    let seen: Vec<(&str, Purpose)> = required
        .iter()
        .map(|component| (component.id.as_str(), component.purpose))
        .collect();
    assert_eq!(
        seen,
        [
            (IBUS, Purpose::TextInput),
            (HUD_HOST_AUTOSTART, Purpose::StatusSurface)
        ]
    );
}

#[test]
fn ibus_in_the_session_with_its_daemon_is_active() {
    assert_eq!(
        status(&Session::ibus("active"), IBUS),
        ComponentStatus::Active
    );
}

#[test]
fn a_session_set_up_for_ibus_whose_daemon_is_gone_has_failed() {
    let session = Session::ibus("gone");
    std::fs::remove_dir_all(session.scratch.path("proc").join("ibus-daemon")).unwrap();
    session.run("pipewire");
    assert_eq!(status(&session, IBUS), ComponentStatus::Failed);
}

#[test]
fn xubuntu_without_ibus_is_unavailable_and_nothing_is_installed() {
    let session = Session::ibus("absent");
    std::fs::remove_file(session.scratch.path("bin").join("ibus-daemon")).unwrap();
    assert_eq!(status(&session, IBUS), ComponentStatus::Unavailable);
    let runner = FakeCommandRunner::default();
    let error = block_on(session.components(&runner).enable(IBUS)).unwrap_err();
    assert!(matches!(error, ComponentError::Failed { .. }), "{error:?}");
    assert!(runner.calls().is_empty());
}

#[test]
fn ibus_installed_since_login_waits_for_a_relogin_once_chosen() {
    let session = Session::ibus("since-login").without_session_env();
    assert_eq!(status(&session, IBUS), ComponentStatus::NeedsRelogin);

    let runner = FakeCommandRunner::scripted([ok()]);
    block_on(session.components(&runner).enable(IBUS)).unwrap();
    assert_eq!(args(&runner), [["im-config", "-n", "ibus"]]);

    session.xinputrc("# im-config(8) generated\nrun_im ibus\n# signature\n");
    assert_eq!(status(&session, IBUS), ComponentStatus::ActiveAfterRelogin);
    // Nothing more to do.
    let runner = FakeCommandRunner::default();
    block_on(session.components(&runner).enable(IBUS)).unwrap();
    assert!(runner.calls().is_empty());
}

#[test]
fn the_im_config_that_dropped_dash_n_is_asked_with_dash_w() {
    let session = Session::ibus("dash-w").without_session_env();
    let runner = FakeCommandRunner::scripted([refused("unknown option -n"), ok()]);
    block_on(session.components(&runner).enable(IBUS)).unwrap();
    assert_eq!(
        args(&runner),
        [["im-config", "-n", "ibus"], ["im-config", "-w", "ibus"]]
    );
}

#[test]
fn an_im_config_that_fails_is_reported_as_the_command() {
    let session = Session::ibus("fails").without_session_env();
    let runner = FakeCommandRunner::scripted([refused("first"), refused("second")]);
    let error = block_on(session.components(&runner).enable(IBUS)).unwrap_err();
    match error {
        ComponentError::Failed {
            kind: StepKind::Command,
            step,
            message,
        } => {
            assert_eq!(step, "im-config -w ibus");
            assert!(message.contains("second"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    let runner = FakeCommandRunner::scripted([Err(CommandError::NotFound {
        executable: "im-config".into(),
    })]);
    let error = block_on(session.components(&runner).enable(IBUS)).unwrap_err();
    assert!(
        matches!(&error, ComponentError::Failed { message, .. } if message.contains("not installed")),
        "{error:?}"
    );
    assert_eq!(runner.calls().len(), 1);
}

#[test]
fn another_input_method_is_left_alone() {
    let session = Session::ibus("fcitx").without_session_env();
    session.xinputrc("run_im fcitx5\n");
    assert_eq!(
        status(&session, IBUS),
        ComponentStatus::Blocked(Blocker::Incompatible)
    );
    let mut session = Session::ibus("fcitx-env").without_session_env();
    session.env.insert("GTK_IM_MODULE".into(), "fcitx".into());
    assert_eq!(
        status(&session, IBUS),
        ComponentStatus::Blocked(Blocker::Incompatible)
    );
    let runner = FakeCommandRunner::default();
    assert!(block_on(session.components(&runner).enable(IBUS)).is_err());
    assert!(runner.calls().is_empty());
}

#[test]
fn a_commented_xinputrc_line_chooses_nothing() {
    let session = Session::ibus("commented").without_session_env();
    session.xinputrc("# run_im fcitx5\n");
    assert_eq!(status(&session, IBUS), ComponentStatus::NeedsRelogin);
}

#[test]
fn the_indicator_host_is_the_autostart_entry_the_package_ships() {
    let session = Session::ibus("hud");
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::Unavailable
    );
    let entry = session.autostart(
        "system",
        "[Desktop Entry]\nType=Application\nExec=myna-hud-host\nOnlyShowIn=XFCE;\n",
    );
    // Installed, but this session started before it was.
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::NeedsRelogin
    );
    session.run("myna-hud-host");
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::Active
    );

    // Xfce's session settings turn an entry off with a user copy.
    session.autostart("user", "[Desktop Entry]\nHidden=true\n");
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::Blocked(Blocker::TurnedOff)
    );
    std::fs::remove_file(session.scratch.path("user").join(HUD_HOST_AUTOSTART)).unwrap();
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::Active
    );

    std::fs::write(&entry, "[Desktop Entry]\nExec=x\nHidden=true\n").unwrap();
    assert_eq!(
        status(&session, HUD_HOST_AUTOSTART),
        ComponentStatus::Blocked(Blocker::TurnedOff)
    );
}

#[test]
fn the_package_not_myna_settings_installs_the_host() {
    let session = Session::ibus("hud-enable");
    let error = block_on(
        session
            .components(&FakeCommandRunner::default())
            .enable(HUD_HOST_AUTOSTART),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            ComponentError::Failed {
                kind: StepKind::Setting,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_component_of_another_desktop_is_unknown() {
    let session = Session::ibus("unknown");
    let components = session.components(&FakeCommandRunner::default());
    assert_eq!(
        block_on(components.status("myna-shell@canonical.com")),
        ComponentStatus::Unavailable
    );
    assert_eq!(
        block_on(components.enable("myna-shell@canonical.com")),
        Err(ComponentError::Unknown("myna-shell@canonical.com".into()))
    );
}

#[test]
fn the_shipped_entry_is_the_one_the_status_looks_for() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(HUD_HOST_AUTOSTART);
    let entry = gio::glib::KeyFile::new();
    entry
        .load_from_file(&path, gio::glib::KeyFileFlags::NONE)
        .expect("the package ships the entry");
    let get = |key: &str| entry.string("Desktop Entry", key).unwrap().to_string();
    assert_eq!(get("OnlyShowIn"), "XFCE;");
    assert_eq!(get("NoDisplay"), "true");
    assert_eq!(get("Exec"), "/usr/libexec/myna-config/myna-hud-host");
    assert!(entry.boolean("Desktop Entry", "Hidden").is_err());
}
