//! The machine as the desktops' component statuses read it, and what they
//! share: IBus as the input method Myna types through.

use std::collections::HashMap;
use std::path::PathBuf;

use gio::glib;
use myna_platform::components::{ComponentError, StepKind};

/// IBus as the active input method.
pub const IBUS: &str = "ibus";

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

/// A command step that failed, or could not be run.
pub fn command_failed(step: &str, message: &str) -> ComponentError {
    ComponentError::Failed {
        kind: StepKind::Command,
        step: step.to_owned(),
        message: message.to_owned(),
    }
}
