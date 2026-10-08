//! Components: the desktop pieces Myna needs, outside its own processes.
//!
//! On GNOME that is the shell extension hosting the status surface; on Xfce
//! an autostart entry for the same job and IBus as the active input method.
//! A backend says where each stands and how far it can bring it.

use async_trait::async_trait;

/// What a component is for, so callers can explain its absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Hosts the dictation status surface.
    StatusSurface,
    /// Makes Myna's text input backend reachable from applications.
    TextInput,
}

/// One component a desktop needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    /// The backend's own name for it: an extension uuid, an autostart file,
    /// an input method framework.
    pub id: String,
    pub purpose: Purpose,
}

/// Why a component cannot be enabled from Myna.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// The user turned off the mechanism that runs it (all extensions).
    TurnedOff,
    /// The administrator does not let the user enable it.
    Locked,
    /// A copy the user installed hides the one Myna ships.
    Shadowed,
    /// The installed copy does not support the running desktop.
    Incompatible,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentStatus {
    Active,
    /// Enabled; it starts at the next login.
    ActiveAfterRelogin,
    /// Installed and stopped; [`Components::enable`] starts it now.
    Inactive,
    /// Installed; `enable` makes it start at the next login.
    NeedsRelogin,
    /// Installed; only the user, outside Myna, can enable it.
    Blocked(Blocker),
    /// It ran and failed.
    Failed,
    /// Not installed, or the desktop cannot be asked.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ComponentError {
    #[error("{0} is not a component of this desktop")]
    Unknown(String),
    #[error("enabling was cancelled")]
    Cancelled,
    /// `step` names what was attempted, such as a D-Bus call or a settings
    /// key, for the failure report.
    #[error("{step} failed: {message}")]
    Failed { step: String, message: String },
}

/// The components one desktop needs.
#[async_trait(?Send)]
pub trait Components {
    /// What this desktop needs, in the order setup should bring them up.
    fn required(&self) -> Vec<Component>;

    async fn status(&self, id: &str) -> ComponentStatus;

    /// Bring the component as far as Myna can without authorization: running
    /// now where the desktop allows, else listed for the next login.
    async fn enable(&self, id: &str) -> Result<(), ComponentError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_names_the_step() {
        let error = ComponentError::Failed {
            step: "org.gnome.Shell.Extensions.EnableExtension".into(),
            message: "no such extension".into(),
        };
        assert_eq!(
            error.to_string(),
            "org.gnome.Shell.Extensions.EnableExtension failed: no such extension"
        );
    }
}
