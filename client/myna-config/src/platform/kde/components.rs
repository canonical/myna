//! IBus on Plasma Wayland: KWin's input method. On Wayland KWin, not the
//! session's environment, runs the input method: the `.desktop` file kwinrc's
//! `[Wayland] InputMethod` names, at once when it changes and at every login.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use myna_platform::components::{Blocker, ComponentError, ComponentStatus};

use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::platform::host::{command_failed as failed, Host, HostComponents, IbusSetup};

pub type KdeComponents = HostComponents<KwinInputMethod>;

#[derive(Default)]
pub struct KwinInputMethod;

#[async_trait(?Send)]
impl IbusSetup for KwinInputMethod {
    /// IBus's own way to become KWin's input method: it writes kwinrc, tells
    /// KWin, and returns once KWin has started it.
    const STEP: &'static str = "ibus start --type kde-wayland";
    const SETTINGS: &'static str = "System Settings";

    fn status(&self, host: &Host) -> ComponentStatus {
        if !host.ibus_installed() {
            return ComponentStatus::Unavailable;
        }
        match input_method(host) {
            Some(entry) if names_ibus(&entry) => {
                if host.running("ibus-daemon") {
                    ComponentStatus::Active
                } else {
                    ComponentStatus::Failed
                }
            }
            Some(_) => ComponentStatus::Blocked(Blocker::Incompatible),
            None if host.running("fcitx5") => ComponentStatus::Blocked(Blocker::Incompatible),
            None => ComponentStatus::Inactive,
        }
    }

    async fn set_up(&self, runner: &dyn CommandRunner) -> Result<(), ComponentError> {
        let mut words = Self::STEP.split(' ').map(str::to_owned);
        let request = CommandRequest::new(words.next().unwrap_or_default(), words.collect())
            .with_timeout(Duration::from_secs(10));
        match runner.run(request, CancellationToken::new()).await {
            Ok(_) => Ok(()),
            Err(CommandError::Cancelled) => Err(ComponentError::Cancelled),
            Err(error) => Err(failed(Self::STEP, &error.to_string())),
        }
    }
}

/// The `.desktop` file KWin runs as the input method: the user's kwinrc over
/// the system's, as KConfig cascades them.
fn input_method(host: &Host) -> Option<String> {
    std::iter::once(&host.config_home)
        .chain(&host.config_dirs)
        .find_map(|dir| {
            let rc = std::fs::read_to_string(dir.join("kwinrc")).ok()?;
            wayland_input_method(&rc).map(str::to_owned)
        })
        .filter(|entry| !entry.is_empty())
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
