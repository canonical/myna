//! What Myna needs from an Xfce session beyond its own processes: IBus as the
//! active input method, so text can reach applications, and the autostart
//! entry that hosts the dictation indicator.
//!
//! Xubuntu does not install IBus, and Myna Settings is an unconfined deb app,
//! so it never installs a package: a missing IBus is reported as such. What it
//! may do is `im-config`, for the user, which takes effect at the next login.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use gio::glib;
use myna_platform::components::{
    Blocker, Component, ComponentError, ComponentStatus, Components, Purpose, StepKind,
};

use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};

/// IBus as the active input method.
pub const IBUS: &str = "ibus";
/// The XDG autostart entry that starts the HUD host with the session. The
/// myna-config deb ships it in `/etc/xdg/autostart`.
// TODO(T6s): the supervisor and this entry land with the HUD host; check the
// name against the file the deb installs.
pub const HUD_HOST_AUTOSTART: &str = "com.canonical.Myna.HudHost.desktop";

/// The machine as the statuses read it.
#[derive(Clone, Debug)]
pub struct Host {
    /// The session's environment: `GTK_IM_MODULE`, `XMODIFIERS`.
    pub env: HashMap<String, String>,
    /// Where programs are looked up.
    pub path: Vec<PathBuf>,
    /// `/proc`.
    pub proc_dir: PathBuf,
    pub home: PathBuf,
    /// The system's autostart directories, most preferred first.
    pub system_autostart: Vec<PathBuf>,
    pub user_autostart: PathBuf,
}

impl Host {
    /// This process's session.
    pub fn current() -> Self {
        let env = ["GTK_IM_MODULE", "XMODIFIERS"]
            .into_iter()
            .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?)))
            .collect();
        Self {
            env,
            path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            proc_dir: PathBuf::from("/proc"),
            home: glib::home_dir(),
            system_autostart: glib::system_config_dirs()
                .into_iter()
                .map(|dir| dir.join("autostart"))
                .collect(),
            user_autostart: glib::user_config_dir().join("autostart"),
        }
    }
}

pub struct XfceComponents {
    host: Host,
    runner: Arc<dyn CommandRunner>,
}

impl XfceComponents {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_host(Host::current(), runner)
    }

    pub fn with_host(host: Host, runner: Arc<dyn CommandRunner>) -> Self {
        Self { host, runner }
    }

    fn ibus_status(&self) -> ComponentStatus {
        if !self.ibus_installed() {
            return ComponentStatus::Unavailable;
        }
        let env = |name: &str| self.host.env.get(name).map(String::as_str).unwrap_or("");
        let session_uses_ibus = env("GTK_IM_MODULE") == "ibus" && env("XMODIFIERS") == "@im=ibus";
        if session_uses_ibus {
            return if self.ibus_running() {
                ComponentStatus::Active
            } else {
                // The session was set up for IBus and its daemon is gone.
                ComponentStatus::Failed
            };
        }
        let other_in_session = !matches!(env("GTK_IM_MODULE"), "" | "ibus");
        match self.chosen_framework() {
            Some(chosen) if chosen == "ibus" => ComponentStatus::ActiveAfterRelogin,
            Some(_) => ComponentStatus::Blocked(Blocker::Incompatible),
            None if other_in_session => ComponentStatus::Blocked(Blocker::Incompatible),
            None => ComponentStatus::NeedsRelogin,
        }
    }

    fn ibus_installed(&self) -> bool {
        self.host
            .path
            .iter()
            .any(|dir| dir.join("ibus-daemon").is_file())
    }

    /// A process named `ibus-daemon` of this user.
    fn ibus_running(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(entries) = std::fs::read_dir(&self.host.proc_dir) else {
            return false;
        };
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        entries.flatten().any(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim() == "ibus-daemon")
                && entry.metadata().is_ok_and(|meta| meta.uid() == uid)
        })
    }

    /// The framework `~/.xinputrc` runs, as `im-config` writes it.
    fn chosen_framework(&self) -> Option<String> {
        let rc = std::fs::read_to_string(self.host.home.join(".xinputrc")).ok()?;
        rc.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| line.strip_prefix("run_im "))
            .map(|name| name.trim().to_owned())
    }

    fn hud_host_status(&self) -> ComponentStatus {
        let installed = self
            .host
            .system_autostart
            .iter()
            .map(|dir| dir.join(HUD_HOST_AUTOSTART))
            .find(|path| path.is_file());
        let Some(entry) = installed else {
            return ComponentStatus::Unavailable;
        };
        // Xfce's session settings write a user copy with Hidden=true to turn
        // an entry off; so does a user override of any autostart entry.
        let user = self.host.user_autostart.join(HUD_HOST_AUTOSTART);
        if user.is_file() && key_file_says(&user, "Hidden", true) {
            return ComponentStatus::Blocked(Blocker::TurnedOff);
        }
        if key_file_says(&entry, "Hidden", true) {
            return ComponentStatus::Blocked(Blocker::TurnedOff);
        }
        ComponentStatus::Active
    }

    /// `im-config` for the user: `-n` where it takes a framework name, `-w`
    /// in the 1.x series that replaced it.
    async fn choose_ibus(&self) -> Result<(), ComponentError> {
        let mut last = None;
        for flag in ["-n", "-w"] {
            let request = CommandRequest::new(
                "im-config".to_owned(),
                vec![flag.to_owned(), IBUS.to_owned()],
            );
            match self.runner.run(request, CancellationToken::new()).await {
                Ok(_) => return Ok(()),
                Err(CommandError::NotFound { .. }) => {
                    return Err(failed("im-config", "im-config is not installed"));
                }
                Err(CommandError::Cancelled) => return Err(ComponentError::Cancelled),
                Err(error) => last = Some((flag, error)),
            }
        }
        let (flag, error) = last.expect("both flags were tried");
        Err(failed(
            &format!("im-config {flag} {IBUS}"),
            &error.to_string(),
        ))
    }
}

fn failed(step: &str, message: &str) -> ComponentError {
    ComponentError::Failed {
        kind: StepKind::Command,
        step: step.to_owned(),
        message: message.to_owned(),
    }
}

/// Whether `[Desktop Entry]` sets `key` to `value`.
fn key_file_says(path: &Path, key: &str, value: bool) -> bool {
    let file = glib::KeyFile::new();
    file.load_from_file(path, glib::KeyFileFlags::NONE).is_ok()
        && file
            .boolean("Desktop Entry", key)
            .is_ok_and(|set| set == value)
}

#[async_trait(?Send)]
impl Components for XfceComponents {
    fn required(&self) -> Vec<Component> {
        vec![
            Component {
                id: IBUS.to_owned(),
                purpose: Purpose::TextInput,
            },
            Component {
                id: HUD_HOST_AUTOSTART.to_owned(),
                purpose: Purpose::StatusSurface,
            },
        ]
    }

    async fn status(&self, id: &str) -> ComponentStatus {
        match id {
            IBUS => self.ibus_status(),
            HUD_HOST_AUTOSTART => self.hud_host_status(),
            _ => ComponentStatus::Unavailable,
        }
    }

    async fn enable(&self, id: &str) -> Result<(), ComponentError> {
        match id {
            IBUS => match self.ibus_status() {
                ComponentStatus::Active | ComponentStatus::ActiveAfterRelogin => Ok(()),
                ComponentStatus::NeedsRelogin => self.choose_ibus().await,
                ComponentStatus::Unavailable => Err(failed("im-config", "IBus is not installed")),
                ComponentStatus::Blocked(_) => Err(failed(
                    "im-config",
                    "another input method is chosen; switch it in the session settings",
                )),
                _ => Err(failed("im-config", "IBus is not running")),
            },
            HUD_HOST_AUTOSTART => Err(ComponentError::Failed {
                kind: StepKind::Setting,
                step: HUD_HOST_AUTOSTART.to_owned(),
                message: "the myna-config package installs it".to_owned(),
            }),
            other => Err(ComponentError::Unknown(other.to_owned())),
        }
    }
}
