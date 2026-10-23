//! The machine as the desktops' component statuses read it, and the
//! components the desktops without a shell extension share: IBus as the input
//! method Myna types through, set up each desktop's way, and the autostart
//! entry that hosts the dictation indicator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use gio::glib;
use myna_platform::components::{
    Blocker, Component, ComponentError, ComponentStatus, Components, Purpose, StepKind,
};

use crate::command::CommandRunner;

/// IBus as the active input method.
pub const IBUS: &str = "ibus";

/// The XDG autostart entry that starts the HUD host with the session. The
/// myna-config deb ships it in `/etc/xdg/autostart` (`data/` here).
pub const HUD_HOST_AUTOSTART: &str = "com.canonical.Myna.HudHost.desktop";
/// The supervisor the entry starts (`hud_host`).
const HUD_HOST_PROCESS: &str = "myna-hud-host";

/// How a desktop makes IBus the input method.
#[async_trait(?Send)]
pub trait IbusSetup: Default {
    /// The step that sets it up, as a failure names it.
    const STEP: &'static str;
    /// Where the user switches away from another input method.
    const SETTINGS: &'static str;

    fn status(&self, host: &Host) -> ComponentStatus;

    async fn set_up(&self, runner: &dyn CommandRunner) -> Result<(), ComponentError>;
}

/// IBus set up `S`'s way, and the HUD host's autostart entry.
pub struct HostComponents<S> {
    host: Host,
    runner: Arc<dyn CommandRunner>,
    ibus: S,
}

impl<S: IbusSetup> HostComponents<S> {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_host(Host::current(), runner)
    }

    pub fn with_host(host: Host, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            host,
            runner,
            ibus: S::default(),
        }
    }
}

#[async_trait(?Send)]
impl<S: IbusSetup> Components for HostComponents<S> {
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
            IBUS => self.ibus.status(&self.host),
            HUD_HOST_AUTOSTART => self.host.hud_host_status(),
            _ => ComponentStatus::Unavailable,
        }
    }

    async fn enable(&self, id: &str) -> Result<(), ComponentError> {
        match id {
            IBUS => {}
            // Myna Settings cannot install the entry; only the package does.
            HUD_HOST_AUTOSTART => {
                return Err(ComponentError::Failed {
                    kind: StepKind::Setting,
                    step: HUD_HOST_AUTOSTART.to_owned(),
                    message: "the myna-config package installs it".to_owned(),
                })
            }
            _ => return Err(ComponentError::Unknown(id.to_owned())),
        }
        let failed = |message: &str| Err(command_failed(S::STEP, message));
        match self.ibus.status(&self.host) {
            ComponentStatus::Active | ComponentStatus::ActiveAfterRelogin => Ok(()),
            ComponentStatus::Inactive | ComponentStatus::NeedsRelogin => {
                self.ibus.set_up(self.runner.as_ref()).await
            }
            ComponentStatus::Unavailable => failed("IBus is not installed"),
            ComponentStatus::Blocked(_) => failed(&format!(
                "another input method is chosen; switch it in {}",
                S::SETTINGS
            )),
            ComponentStatus::Failed => failed("IBus is not running"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Host {
    /// The session's environment: `GTK_IM_MODULE`, `XMODIFIERS`.
    pub env: HashMap<String, String>,
    /// Where programs are looked up.
    pub path: Vec<PathBuf>,
    /// `/proc`.
    pub proc_dir: PathBuf,
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`.
    pub config_home: PathBuf,
    /// `$XDG_CONFIG_DIRS`, most preferred first.
    pub config_dirs: Vec<PathBuf>,
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
            config_home: glib::user_config_dir(),
            config_dirs: glib::system_config_dirs(),
        }
    }

    pub fn ibus_installed(&self) -> bool {
        self.path
            .iter()
            .any(|dir| dir.join("ibus-daemon").is_file())
    }

    pub fn hud_host_status(&self) -> ComponentStatus {
        let installed = self
            .config_dirs
            .iter()
            .map(|dir| dir.join("autostart").join(HUD_HOST_AUTOSTART))
            .find(|path| path.is_file());
        let Some(entry) = installed else {
            return ComponentStatus::Unavailable;
        };
        // Xfce's session settings and Plasma's autostart page write a user
        // copy with Hidden=true to turn an entry off.
        let user = self.config_home.join("autostart").join(HUD_HOST_AUTOSTART);
        if (user.is_file() && key_file_says(&user, "Hidden", true))
            || key_file_says(&entry, "Hidden", true)
        {
            return ComponentStatus::Blocked(Blocker::TurnedOff);
        }
        // The entry starts the supervisor with the session, so one installed
        // mid-session has not run yet.
        if self.running(HUD_HOST_PROCESS) {
            ComponentStatus::Active
        } else {
            ComponentStatus::NeedsRelogin
        }
    }

    /// A process of this user whose `comm` is `name`.
    pub fn running(&self, name: &str) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(entries) = std::fs::read_dir(&self.proc_dir) else {
            return false;
        };
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        entries.flatten().any(|entry| {
            std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|comm| comm.trim() == name)
                && entry.metadata().is_ok_and(|meta| meta.uid() == uid)
        })
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

/// A command step that failed, or could not be run.
pub fn command_failed(step: &str, message: &str) -> ComponentError {
    ComponentError::Failed {
        kind: StepKind::Command,
        step: step.to_owned(),
        message: message.to_owned(),
    }
}
