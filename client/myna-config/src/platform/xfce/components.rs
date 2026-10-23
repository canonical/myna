//! IBus on Xfce: the active input method when the session's environment
//! selects it, which `im-config` arranges for the next login.
//!
//! Xubuntu does not install IBus, and Myna Settings is an unconfined deb app,
//! so it never installs a package: a missing IBus is reported as such. What it
//! may do is `im-config`, for the user, which takes effect at the next login.

use async_trait::async_trait;
use myna_platform::components::{Blocker, ComponentError, ComponentStatus};

use crate::command::{CancellationToken, CommandError, CommandRequest, CommandRunner};
use crate::platform::host::{command_failed as failed, Host, HostComponents, IbusSetup, IBUS};

pub type XfceComponents = HostComponents<ImConfig>;

#[derive(Default)]
pub struct ImConfig;

#[async_trait(?Send)]
impl IbusSetup for ImConfig {
    const STEP: &'static str = "im-config";
    const SETTINGS: &'static str = "the session settings";

    fn status(&self, host: &Host) -> ComponentStatus {
        if !host.ibus_installed() {
            return ComponentStatus::Unavailable;
        }
        let env = |name: &str| host.env.get(name).map(String::as_str).unwrap_or("");
        let session_uses_ibus = env("GTK_IM_MODULE") == "ibus" && env("XMODIFIERS") == "@im=ibus";
        if session_uses_ibus {
            return if host.running("ibus-daemon") {
                ComponentStatus::Active
            } else {
                // The session was set up for IBus and its daemon is gone.
                ComponentStatus::Failed
            };
        }
        let other_in_session = !matches!(env("GTK_IM_MODULE"), "" | "ibus");
        match chosen_framework(host) {
            Some(chosen) if chosen == "ibus" => ComponentStatus::ActiveAfterRelogin,
            Some(_) => ComponentStatus::Blocked(Blocker::Incompatible),
            None if other_in_session => ComponentStatus::Blocked(Blocker::Incompatible),
            None => ComponentStatus::NeedsRelogin,
        }
    }

    /// `im-config` for the user: `-n` where it takes a framework name, `-w`
    /// in the 1.x series that replaced it.
    async fn set_up(&self, runner: &dyn CommandRunner) -> Result<(), ComponentError> {
        let mut last = None;
        for flag in ["-n", "-w"] {
            let request = CommandRequest::new(
                "im-config".to_owned(),
                vec![flag.to_owned(), IBUS.to_owned()],
            );
            match runner.run(request, CancellationToken::new()).await {
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

/// The framework `~/.xinputrc` runs, as `im-config` writes it.
fn chosen_framework(host: &Host) -> Option<String> {
    let rc = std::fs::read_to_string(host.home.join(".xinputrc")).ok()?;
    rc.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix("run_im "))
        .map(|name| name.trim().to_owned())
}
