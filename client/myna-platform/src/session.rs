//! Which desktop session this process runs in, and the [`Profile`] that picks
//! its backends.
//!
//! Detection is a pure function of a [`SessionEnv`] snapshot, so every rule is
//! table-tested; [`SessionEnv::from_process`] is the only part that reads the
//! environment or the filesystem.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The display protocol the session speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Wayland,
    X11,
    /// Neither a display variable nor `XDG_SESSION_TYPE` says.
    Unknown,
}

/// The desktop environment, from `XDG_CURRENT_DESKTOP`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Desktop {
    Gnome,
    Xfce,
    /// Anything else, as the variable spelled it; empty when it is unset.
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub kind: SessionKind,
    pub desktop: Desktop,
}

/// The variables detection reads. Empty values count as unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionEnv {
    /// `XDG_CURRENT_DESKTOP`.
    pub current_desktop: Option<String>,
    /// `XDG_SESSION_TYPE`.
    pub session_type: Option<String>,
    /// `WAYLAND_DISPLAY`, dropped by [`SessionEnv::from_process`] when its
    /// socket is gone.
    pub wayland_display: Option<String>,
    /// `DISPLAY`.
    pub display: Option<String>,
    /// `MYNA_PLATFORM`, the profile override for tests.
    pub platform: Option<String>,
}

impl SessionEnv {
    /// Read the variables through `var`.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        Self {
            current_desktop: get("XDG_CURRENT_DESKTOP"),
            session_type: get("XDG_SESSION_TYPE"),
            wayland_display: get("WAYLAND_DISPLAY"),
            display: get("DISPLAY"),
            platform: get("MYNA_PLATFORM"),
        }
    }

    /// This process's environment, without a `WAYLAND_DISPLAY` whose socket
    /// no longer exists.
    pub fn from_process() -> Self {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        Self::from_vars(|name| std::env::var(name).ok())
            .without_dead_wayland(runtime_dir.as_deref(), Path::exists)
    }

    /// Drop `WAYLAND_DISPLAY` when `exists` says its socket is gone. A
    /// relative name with no runtime directory is kept: it cannot be checked.
    pub fn without_dead_wayland(
        mut self,
        runtime_dir: Option<&Path>,
        exists: impl Fn(&Path) -> bool,
    ) -> Self {
        let socket = self
            .wayland_display
            .as_deref()
            .and_then(|display| wayland_socket(display, runtime_dir));
        if socket.is_some_and(|socket| !exists(&socket)) {
            self.wayland_display = None;
        }
        self
    }
}

/// Where the compositor's socket for `display` lives; `None` when a relative
/// name has no runtime directory to resolve against.
pub fn wayland_socket(display: &str, runtime_dir: Option<&Path>) -> Option<PathBuf> {
    let display = Path::new(display);
    if display.is_absolute() {
        Some(display.to_path_buf())
    } else {
        runtime_dir.map(|dir| dir.join(display))
    }
}

impl Session {
    /// A live Wayland display beats `XDG_SESSION_TYPE`, which a session
    /// started as X11 and later re-exported may still carry; a bare `DISPLAY`
    /// means X11 even in a Wayland session, as that is all this process can
    /// reach. The session type decides only when neither display is set, as
    /// for a user service started before the session exported them.
    pub fn detect(env: &SessionEnv) -> Self {
        let kind = if env.wayland_display.is_some() {
            SessionKind::Wayland
        } else if env.display.is_some() {
            SessionKind::X11
        } else {
            match env.session_type.as_deref().map(str::trim) {
                Some(t) if t.eq_ignore_ascii_case("wayland") => SessionKind::Wayland,
                Some(t) if t.eq_ignore_ascii_case("x11") => SessionKind::X11,
                _ => SessionKind::Unknown,
            }
        };
        Self {
            kind,
            desktop: Desktop::parse(env.current_desktop.as_deref().unwrap_or("")),
        }
    }
}

impl Desktop {
    /// `XDG_CURRENT_DESKTOP` is a `:`-separated list, most specific first
    /// (`ubuntu:GNOME`); the first entry Myna knows names the desktop.
    pub fn parse(current_desktop: &str) -> Self {
        current_desktop
            .split(':')
            .map(str::trim)
            .find_map(|entry| {
                if entry.eq_ignore_ascii_case("gnome") {
                    Some(Self::Gnome)
                } else if entry.eq_ignore_ascii_case("xfce") {
                    Some(Self::Xfce)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| Self::Other(current_desktop.trim().to_owned()))
    }
}

/// The set of backends a process composes. Resolved once, at the
/// composition root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Gnome,
    /// Xfce on X11; its status surface host is an X11 client.
    Xfce,
    /// Whatever needs no particular desktop. Every unknown session gets this,
    /// never a desktop-specific profile.
    Generic,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Gnome, Profile::Xfce, Profile::Generic];

    /// The profile `session` gets with no override.
    pub fn for_session(session: &Session) -> Self {
        match (&session.desktop, session.kind) {
            (Desktop::Gnome, _) => Self::Gnome,
            (Desktop::Xfce, SessionKind::Wayland) => Self::Generic,
            (Desktop::Xfce, _) => Self::Xfce,
            (Desktop::Other(_), _) => Self::Generic,
        }
    }

    /// `MYNA_PLATFORM` when set, else the detected session's profile. An
    /// override that names no profile is an error, not a fallback.
    pub fn select(env: &SessionEnv) -> Result<Self, UnknownProfile> {
        match env.platform.as_deref() {
            Some(name) => name.parse(),
            None => Ok(Self::for_session(&Session::detect(env))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Gnome => "gnome",
            Self::Xfce => "xfce",
            Self::Generic => "generic",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MYNA_PLATFORM={0:?} names no profile (gnome, xfce, generic)")]
pub struct UnknownProfile(pub String);

impl FromStr for Profile {
    type Err = UnknownProfile;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.name().eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| UnknownProfile(name.to_owned()))
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
    fn the_session_kind_follows_the_live_display_first() {
        use SessionKind::*;
        let cases: [(&[(&str, &str)], SessionKind); 11] = [
            (&[("WAYLAND_DISPLAY", "wayland-0")], Wayland),
            (
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_SESSION_TYPE", "x11"),
                ],
                Wayland,
            ),
            (
                &[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")],
                Wayland,
            ),
            (&[("DISPLAY", ":0")], X11),
            (&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "wayland")], X11),
            (&[("XDG_SESSION_TYPE", "wayland")], Wayland),
            (&[("XDG_SESSION_TYPE", "X11")], X11),
            (&[("XDG_SESSION_TYPE", "tty")], Unknown),
            (
                &[("WAYLAND_DISPLAY", " "), ("XDG_SESSION_TYPE", "x11")],
                X11,
            ),
            (&[("DISPLAY", ""), ("XDG_SESSION_TYPE", "")], Unknown),
            (&[], Unknown),
        ];
        for (vars, kind) in cases {
            assert_eq!(Session::detect(&env(vars)).kind, kind, "{vars:?}");
        }
    }

    #[test]
    fn the_desktop_is_the_first_entry_myna_knows() {
        let cases = [
            ("GNOME", Desktop::Gnome),
            ("ubuntu:GNOME", Desktop::Gnome),
            ("gnome", Desktop::Gnome),
            ("GNOME-Classic:GNOME", Desktop::Gnome),
            ("XFCE", Desktop::Xfce),
            ("xfce", Desktop::Xfce),
            ("XFCE:GNOME", Desktop::Xfce),
            (" ubuntu : xfce ", Desktop::Xfce),
            ("KDE", Desktop::Other("KDE".into())),
            ("X-Cinnamon", Desktop::Other("X-Cinnamon".into())),
            ("GNOME-Flashback", Desktop::Other("GNOME-Flashback".into())),
            ("", Desktop::Other(String::new())),
        ];
        for (value, desktop) in cases {
            assert_eq!(Desktop::parse(value), desktop, "{value:?}");
        }
        assert_eq!(
            Session::detect(&env(&[])).desktop,
            Desktop::Other(String::new())
        );
    }

    #[test]
    fn unknown_desktops_get_the_generic_profile_never_gnome() {
        use Profile::*;
        let cases: [(&[(&str, &str)], Profile); 9] = [
            (
                &[
                    ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
                    ("WAYLAND_DISPLAY", "w"),
                ],
                Gnome,
            ),
            (
                &[("XDG_CURRENT_DESKTOP", "GNOME"), ("DISPLAY", ":0")],
                Gnome,
            ),
            (&[("XDG_CURRENT_DESKTOP", "GNOME")], Gnome),
            (&[("XDG_CURRENT_DESKTOP", "XFCE"), ("DISPLAY", ":0")], Xfce),
            (&[("XDG_CURRENT_DESKTOP", "XFCE")], Xfce),
            (
                &[("XDG_CURRENT_DESKTOP", "XFCE"), ("WAYLAND_DISPLAY", "w")],
                Generic,
            ),
            (
                &[("XDG_CURRENT_DESKTOP", "KDE"), ("WAYLAND_DISPLAY", "w")],
                Generic,
            ),
            (
                &[
                    ("XDG_CURRENT_DESKTOP", "GNOME-Flashback"),
                    ("DISPLAY", ":0"),
                ],
                Generic,
            ),
            (&[], Generic),
        ];
        for (vars, profile) in cases {
            assert_eq!(Profile::select(&env(vars)), Ok(profile), "{vars:?}");
        }
    }

    #[test]
    fn myna_platform_overrides_detection() {
        let gnome = [("XDG_CURRENT_DESKTOP", "GNOME"), ("WAYLAND_DISPLAY", "w")];
        for (name, profile) in [
            ("xfce", Profile::Xfce),
            ("Generic", Profile::Generic),
            (" GNOME ", Profile::Gnome),
        ] {
            let vars = [gnome[0], gnome[1], ("MYNA_PLATFORM", name)];
            assert_eq!(Profile::select(&env(&vars)), Ok(profile), "{name}");
        }
        let bogus = [gnome[0], ("MYNA_PLATFORM", "kde")];
        assert_eq!(
            Profile::select(&env(&bogus)),
            Err(UnknownProfile("kde".into()))
        );
        let empty = [gnome[0], ("MYNA_PLATFORM", "")];
        assert_eq!(Profile::select(&env(&empty)), Ok(Profile::Gnome));
    }

    #[test]
    fn profile_names_round_trip() {
        for profile in Profile::ALL {
            assert_eq!(profile.name().parse(), Ok(profile));
            assert_eq!(profile.to_string(), profile.name());
        }
    }

    #[test]
    fn a_relative_wayland_display_resolves_in_the_runtime_dir() {
        let run = Path::new("/run/user/1000");
        assert_eq!(
            wayland_socket("wayland-0", Some(run)),
            Some(run.join("wayland-0"))
        );
        assert_eq!(
            wayland_socket("/tmp/w", Some(run)),
            Some(PathBuf::from("/tmp/w"))
        );
        assert_eq!(wayland_socket("wayland-0", None), None);
    }

    #[test]
    fn a_dead_wayland_socket_does_not_make_a_wayland_session() {
        let run = Path::new("/run/user/1000");
        let vars = [
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_SESSION_TYPE", "x11"),
        ];
        let live = env(&vars).without_dead_wayland(Some(run), |p| p == run.join("wayland-0"));
        assert_eq!(Session::detect(&live).kind, SessionKind::Wayland);
        let dead = env(&vars).without_dead_wayland(Some(run), |_| false);
        assert_eq!(dead.wayland_display, None);
        assert_eq!(Session::detect(&dead).kind, SessionKind::X11);
        let unchecked = env(&vars).without_dead_wayland(None, |_| false);
        assert_eq!(unchecked.wayland_display.as_deref(), Some("wayland-0"));
    }
}
