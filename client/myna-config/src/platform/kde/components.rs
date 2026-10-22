//! What Myna needs from a Plasma Wayland session beyond its own processes:
//! IBus as KWin's input method. On Wayland KWin, not the session's
//! environment, runs the input method: the `.desktop` file kwinrc's
//! `[Wayland] InputMethod` names, at once when it changes and at every login.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use myna_platform::components::{
    Blocker, Component, ComponentError, ComponentStatus, Components, Purpose,
};

use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::platform::host::{command_failed as failed, Host, IBUS};

/// IBus's own way to become KWin's input method: it writes kwinrc, tells
/// KWin, and returns once KWin has started it.
const START_IBUS: [&str; 4] = ["ibus", "start", "--type", "kde-wayland"];

pub struct KdeComponents {
    host: Host,
    runner: Arc<dyn CommandRunner>,
}

impl KdeComponents {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_host(Host::current(), runner)
    }

    pub fn with_host(host: Host, runner: Arc<dyn CommandRunner>) -> Self {
        Self { host, runner }
    }

    fn ibus_status(&self) -> ComponentStatus {
        if !self.host.ibus_installed() {
            return ComponentStatus::Unavailable;
        }
        match self.input_method() {
            Some(entry) if names_ibus(&entry) => {
                if self.host.running("ibus-daemon") {
                    ComponentStatus::Active
                } else {
                    ComponentStatus::Failed
                }
            }
            Some(_) => ComponentStatus::Blocked(Blocker::Incompatible),
            None if self.host.running("fcitx5") => ComponentStatus::Blocked(Blocker::Incompatible),
            None => ComponentStatus::Inactive,
        }
    }

    /// The `.desktop` file KWin runs as the input method: the user's kwinrc
    /// over the system's, as KConfig cascades them.
    fn input_method(&self) -> Option<String> {
        std::iter::once(&self.host.config_home)
            .chain(&self.host.config_dirs)
            .find_map(|dir| {
                let rc = std::fs::read_to_string(dir.join("kwinrc")).ok()?;
                wayland_input_method(&rc).map(str::to_owned)
            })
            .filter(|entry| !entry.is_empty())
    }

    async fn start_ibus(&self) -> Result<(), ComponentError> {
        let [executable, arguments @ ..] = START_IBUS;
        let request = CommandRequest::new(
            executable.to_owned(),
            arguments.iter().map(|&word| word.to_owned()).collect(),
        )
        .with_timeout(Duration::from_secs(10));
        match self.runner.run(request, CancellationToken::new()).await {
            Ok(_) => Ok(()),
            Err(CommandError::Cancelled) => Err(ComponentError::Cancelled),
            Err(error) => Err(failed(&START_IBUS.join(" "), &error.to_string())),
        }
    }
}

/// `[Wayland] InputMethod` in one kwinrc, whatever its KConfig flags
/// (`InputMethod[$e]`).
fn wayland_input_method(rc: &str) -> Option<&str> {
    let mut wayland = false;
    for line in rc.lines().map(str::trim) {
        if line.starts_with('[') {
            wayland = line == "[Wayland]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| wayland) else {
            continue;
        };
        let name = key.split_once('[').map_or(key, |(name, _)| name);
        if name.trim() == "InputMethod" {
            return Some(value.trim());
        }
    }
    None
}

fn names_ibus(entry: &str) -> bool {
    Path::new(entry)
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase().contains("ibus"))
}

#[async_trait(?Send)]
impl Components for KdeComponents {
    fn required(&self) -> Vec<Component> {
        vec![Component {
            id: IBUS.to_owned(),
            purpose: Purpose::TextInput,
        }]
    }

    async fn status(&self, id: &str) -> ComponentStatus {
        match id {
            IBUS => self.ibus_status(),
            _ => ComponentStatus::Unavailable,
        }
    }

    async fn enable(&self, id: &str) -> Result<(), ComponentError> {
        if id != IBUS {
            return Err(ComponentError::Unknown(id.to_owned()));
        }
        let step = START_IBUS.join(" ");
        match self.ibus_status() {
            ComponentStatus::Active => Ok(()),
            ComponentStatus::Inactive => self.start_ibus().await,
            ComponentStatus::Unavailable => Err(failed(&step, "IBus is not installed")),
            ComponentStatus::Blocked(_) => Err(failed(
                &step,
                "another input method is chosen; switch it in System Settings",
            )),
            _ => Err(failed(&step, "IBus is not running")),
        }
    }
}
