//! What Myna needs from an Xfce session beyond its own processes: IBus as the
//! active input method, so text can reach applications, and the autostart
//! entry that hosts the dictation indicator (`platform::host`).
//!
//! Xubuntu does not install IBus, and Myna Settings is an unconfined deb app,
//! so it never installs a package: a missing IBus is reported as such. What it
//! may do is `im-config`, for the user, which takes effect at the next login.

use std::sync::Arc;

use async_trait::async_trait;
use myna_platform::components::{
    Blocker, Component, ComponentError, ComponentStatus, Components, Purpose,
};

use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::platform::host::command_failed as failed;
use crate::platform::host::{enable_hud_host, hud_host};
pub use crate::platform::host::{Host, HUD_HOST_AUTOSTART, IBUS};

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
        if !self.host.ibus_installed() {
            return ComponentStatus::Unavailable;
        }
        let env = |name: &str| self.host.env.get(name).map(String::as_str).unwrap_or("");
        let session_uses_ibus = env("GTK_IM_MODULE") == "ibus" && env("XMODIFIERS") == "@im=ibus";
        if session_uses_ibus {
            return if self.host.running("ibus-daemon") {
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

    /// The framework `~/.xinputrc` runs, as `im-config` writes it.
    fn chosen_framework(&self) -> Option<String> {
        let rc = std::fs::read_to_string(self.host.home.join(".xinputrc")).ok()?;
        rc.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| line.strip_prefix("run_im "))
            .map(|name| name.trim().to_owned())
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

#[async_trait(?Send)]
impl Components for XfceComponents {
    fn required(&self) -> Vec<Component> {
        vec![
            Component {
                id: IBUS.to_owned(),
                purpose: Purpose::TextInput,
            },
            hud_host(),
        ]
    }

    async fn status(&self, id: &str) -> ComponentStatus {
        match id {
            IBUS => self.ibus_status(),
            HUD_HOST_AUTOSTART => self.host.hud_host_status(),
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
            HUD_HOST_AUTOSTART => enable_hud_host(),
            other => Err(ComponentError::Unknown(other.to_owned())),
        }
    }
}
