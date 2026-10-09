//! The HUD's composition root: the session it runs in and the [`Profile`]
//! chosen from it, resolved once in `main` and handed down.
//!
//! The environment is read here and nowhere else. The HUD is started by
//! gnome-shell's extension or by `myna-hud-host`, both inside the session, so
//! its own environment names the desktop, unlike the snap daemon's.

use myna_platform::{Profile, Session, SessionEnv};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub session: Session,
    pub profile: Profile,
}

impl Platform {
    /// `MYNA_PLATFORM` overrides the profile for tests; a value naming no
    /// profile is reported and read as the generic one.
    pub fn select(env: &SessionEnv) -> Self {
        let profile = Profile::select(env).unwrap_or_else(|error| {
            eprintln!("myna-hud: {error}");
            Profile::Generic
        });
        Self {
            session: Session::detect(env),
            profile,
        }
    }

    pub fn current() -> Self {
        Self::select(&SessionEnv::from_process())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> SessionEnv {
        SessionEnv::from_vars(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn the_profile_follows_the_session() {
        let xfce = Platform::select(&env(&[("XDG_CURRENT_DESKTOP", "XFCE"), ("DISPLAY", ":0")]));
        assert_eq!(xfce.profile, Profile::Xfce);
        assert_eq!(xfce.session.kind, myna_platform::SessionKind::X11);
    }

    #[test]
    fn a_bad_override_is_generic_not_a_fallback_to_the_desktop() {
        let platform = Platform::select(&env(&[
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("MYNA_PLATFORM", "kde"),
        ]));
        assert_eq!(platform.profile, Profile::Generic);
    }
}
