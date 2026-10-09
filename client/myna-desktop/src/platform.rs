//! The daemon's composition root: the [`Profile`] of the session, resolved
//! once when a decision first needs it.
//!
//! The daemon needs none at startup. Its text input is IBus and its fallback
//! indicator is the freedesktop notification service on every profile, so
//! only `--install-shortcut` varies, and that is run by the user from a
//! terminal in the session.
//!
//! The daemon proper must not read its own environment for the session: the
//! snap's user service starts before the session exports `DISPLAY`,
//! `XDG_SESSION_TYPE` and `XDG_CURRENT_DESKTOP`, so there they are absent and
//! every session would read as generic. The user manager's environment has
//! them, but the snap's profile grants no access to it on any bus.
//! `client/.kb/platform-layer.md` has the measurements and the options left
//! if the daemon ever needs the session at runtime.

use myna_platform::{Profile, SessionEnv};

pub struct Platform {
    profile: Profile,
}

impl Platform {
    /// `MYNA_PLATFORM` overrides the profile for tests; a value naming no
    /// profile is reported and read as the generic one.
    pub fn select(env: &SessionEnv) -> Self {
        let profile = Profile::select(env).unwrap_or_else(|error| {
            eprintln!("myna-desktop: {error}");
            Profile::Generic
        });
        Self { profile }
    }

    /// The platform of the calling process, for commands run in the session.
    pub fn current() -> Self {
        Self::select(&SessionEnv::from_process())
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// Why `--install-shortcut` cannot bind `command` here, `None` where it
    /// can. It writes a GNOME media-keys entry; Xfce's xfconf entry is
    /// Myna Settings' (a GTK crate the daemon does not link).
    pub fn install_shortcut_refusal(&self, command: &str) -> Option<String> {
        let session = match self.profile {
            Profile::Gnome => return None,
            Profile::Xfce => "an Xfce session",
            Profile::Generic => {
                "a session not recognised as GNOME (set MYNA_PLATFORM=gnome to force it)"
            }
        };
        Some(format!(
            "--install-shortcut binds a GNOME shortcut and this is {session}.\n\
             Bind the key in Myna Settings, or in your desktop's keyboard settings with the command `{command}`."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(vars: &[(&str, &str)]) -> Platform {
        Platform::select(&SessionEnv::from_vars(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }))
    }

    #[test]
    fn gnome_binds_the_shortcut_itself() {
        let gnome = platform(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")]);
        assert_eq!(gnome.profile(), Profile::Gnome);
        assert_eq!(gnome.install_shortcut_refusal("x"), None);
    }

    #[test]
    fn other_desktops_point_to_settings_with_the_command() {
        for vars in [
            &[("XDG_CURRENT_DESKTOP", "XFCE"), ("DISPLAY", ":0")][..],
            &[("XDG_CURRENT_DESKTOP", "KDE")][..],
            &[][..],
        ] {
            let message = platform(vars)
                .install_shortcut_refusal("/snap/bin/myna.toggle")
                .unwrap_or_else(|| panic!("{vars:?}"));
            assert!(message.contains("Myna Settings"), "{message}");
            assert!(message.contains("`/snap/bin/myna.toggle`"), "{message}");
        }
    }

    #[test]
    fn a_bad_override_is_generic() {
        let bad = platform(&[("XDG_CURRENT_DESKTOP", "GNOME"), ("MYNA_PLATFORM", "kde")]);
        assert_eq!(bad.profile(), Profile::Generic);
    }
}
