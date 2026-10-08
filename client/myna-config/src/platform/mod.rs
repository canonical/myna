//! Myna Settings' backends for the desktop it runs on, and the one place that
//! picks them.
//!
//! The rest of the application names `myna-platform` contracts, never a
//! desktop: [`Platform`] is built once from the session's [`Profile`].

use std::rc::Rc;

use myna_platform::activation::Activation;
use myna_platform::{Profile, SessionEnv};

pub mod gnome;
pub mod xfce;

/// The backends of one desktop, as Settings uses them.
pub struct Platform {
    profile: Profile,
    activation: Option<Rc<dyn Activation>>,
}

impl Platform {
    /// The backends `profile` names.
    pub fn for_profile(profile: Profile) -> Self {
        let activation: Option<Rc<dyn Activation>> = match profile {
            Profile::Gnome => gnome::activation::GnomeActivation::open()
                .map(|activation| Rc::new(activation) as Rc<dyn Activation>),
            Profile::Xfce => xfce::activation::XfconfActivation::open()
                .map(|activation| Rc::new(activation) as Rc<dyn Activation>)
                .map_err(|error| {
                    gio::glib::g_warning!(crate::LOG_DOMAIN, "no Xfce shortcuts: {error}")
                })
                .ok(),
            Profile::Generic => None,
        };
        Self {
            profile,
            activation,
        }
    }

    /// The profile of this process's session. `MYNA_PLATFORM` overrides it for
    /// tests; a value naming no profile is reported and read as the generic
    /// one.
    pub fn select(env: &SessionEnv) -> Self {
        let profile = Profile::select(env).unwrap_or_else(|error| {
            gio::glib::g_warning!(crate::LOG_DOMAIN, "{error}");
            Profile::Generic
        });
        Self::for_profile(profile)
    }

    /// This process's platform, built on first use.
    pub fn current() -> Rc<Self> {
        thread_local! {
            static CURRENT: Rc<Platform> = Rc::new(Platform::select(&SessionEnv::from_process()));
        }
        CURRENT.with(Rc::clone)
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The dictation shortcut, where the desktop has one Myna can manage.
    pub fn activation(&self) -> Option<Rc<dyn Activation>> {
        self.activation.clone()
    }
}
