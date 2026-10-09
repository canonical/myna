//! Myna Settings' backends for the desktop it runs on, and the one place that
//! picks them.
//!
//! The rest of the application names `myna-platform` contracts, never a
//! desktop: [`Platform`] is built once from the session's [`Profile`].

use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use libadwaita as adw;
use myna_platform::activation::Activation;
use myna_platform::appearance::Appearance;
use myna_platform::components::{
    Component, ComponentError, ComponentStatus, Components, Purpose, StepKind,
};
use myna_platform::{Profile, SessionEnv};

use crate::onboarding::ExtensionReport;
use crate::ports::{FailedStep, SystemConfiguratorError};

pub mod appearance;
pub mod gnome;
pub mod xfce;

/// The backends of one desktop, as Settings uses them.
pub struct Platform {
    profile: Profile,
    activation: Option<Rc<dyn Activation>>,
    components: Rc<dyn Components>,
    appearance: Rc<dyn Appearance>,
}

impl Platform {
    /// The backends `profile` names.
    pub fn for_profile(profile: Profile) -> Self {
        let (activation, components): (Option<Rc<dyn Activation>>, Rc<dyn Components>) =
            match profile {
                Profile::Gnome => (
                    gnome::activation::GnomeActivation::open()
                        .map(|activation| Rc::new(activation) as Rc<dyn Activation>),
                    Rc::new(gnome::components::GnomeComponents::new()),
                ),
                Profile::Xfce => (
                    xfce::activation::XfconfActivation::open()
                        .map(|activation| Rc::new(activation) as Rc<dyn Activation>)
                        .map_err(|error| {
                            gio::glib::g_warning!(crate::LOG_DOMAIN, "no Xfce shortcuts: {error}")
                        })
                        .ok(),
                    Rc::new(xfce::components::XfceComponents::new(Arc::new(
                        crate::command::GioCommandRunner,
                    ))),
                ),
                Profile::Generic => (None, Rc::new(NoComponents)),
            };
        let appearance: Rc<dyn Appearance> = match profile {
            Profile::Gnome => Rc::new(gnome::appearance::GnomeAppearance),
            Profile::Xfce => Rc::new(match xfce::xfconf::Xfconf::session("xsettings") {
                Ok(xfconf) => appearance::GtkAppearance::with_xfconf(xfconf),
                Err(_) => appearance::GtkAppearance::default(),
            }),
            Profile::Generic => Rc::new(appearance::GtkAppearance::default()),
        };
        Self {
            profile,
            activation,
            components,
            appearance,
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

    /// The desktop's reduced-motion and high-contrast preferences.
    pub fn appearance(&self) -> Rc<dyn Appearance> {
        self.appearance.clone()
    }

    /// The colour scheme libadwaita should take. On GNOME its settings portal
    /// drives it already; elsewhere nothing does, so the desktop's theme is
    /// fed in (Xfce's `Greybird-dark` would otherwise stay light).
    pub fn color_scheme(&self) -> adw::ColorScheme {
        match self.profile {
            Profile::Gnome => adw::ColorScheme::Default,
            Profile::Xfce | Profile::Generic if self.appearance.read().prefers_dark => {
                adw::ColorScheme::ForceDark
            }
            Profile::Xfce | Profile::Generic => adw::ColorScheme::Default,
        }
    }

    /// What this desktop needs outside Myna's own processes.
    pub fn components(&self) -> Rc<dyn Components> {
        self.components.clone()
    }

    /// What Diagnostics says of the desktop's pieces: gnome-shell's own
    /// account of the extension where there is one, else each required
    /// component's status.
    pub fn diagnostics(&self) -> DesktopDiagnostics {
        if self.profile == Profile::Gnome {
            return DesktopDiagnostics {
                extension: Some(gnome::components::extension_report(
                    crate::onboarding::SHELL_EXTENSION_UUID,
                )),
                components: Vec::new(),
            };
        }
        let components = self
            .components
            .required()
            .into_iter()
            .map(|component| ComponentFact {
                purpose: component.purpose,
                status: immediately(self.components.status(&component.id))
                    .unwrap_or(ComponentStatus::Unavailable),
            })
            .collect();
        DesktopDiagnostics {
            extension: None,
            components,
        }
    }
}

/// One component's status, as Diagnostics reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentFact {
    pub purpose: Purpose,
    pub status: ComponentStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopDiagnostics {
    pub extension: Option<ExtensionReport>,
    pub components: Vec<ComponentFact>,
}

/// A desktop Myna knows nothing about needs nothing from it.
struct NoComponents;

#[async_trait(?Send)]
impl Components for NoComponents {
    fn required(&self) -> Vec<Component> {
        Vec::new()
    }

    async fn status(&self, _id: &str) -> ComponentStatus {
        ComponentStatus::Unavailable
    }

    async fn enable(&self, id: &str) -> Result<(), ComponentError> {
        Err(ComponentError::Unknown(id.to_owned()))
    }
}

/// `future`'s output if it is ready without waiting on anything. The Xfce
/// statuses read files and answer at once; Diagnostics builds its page
/// synchronously and must not spin the main loop to wait for one.
fn immediately<T>(future: impl Future<Output = T>) -> Option<T> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A failed component step as the failure report names it: a call as a
/// D-Bus call, a setting as a setting, a command with its executable.
pub fn step_failure(error: ComponentError) -> SystemConfiguratorError {
    match error {
        ComponentError::Cancelled => SystemConfiguratorError::Cancelled,
        ComponentError::Unknown(id) => SystemConfiguratorError::Execution {
            step: FailedStep::Setting { key: id.clone() },
            message: format!("{id} is not a component of this desktop"),
        },
        ComponentError::Failed {
            kind,
            step,
            message,
        } => match kind {
            StepKind::Call => SystemConfiguratorError::dbus_execution(step, message),
            StepKind::Setting => SystemConfiguratorError::setting_execution(step, message),
            StepKind::Command => {
                let mut words = step.split_whitespace().map(str::to_owned);
                let executable = words.next().unwrap_or_default();
                SystemConfiguratorError::execution(
                    executable,
                    words.collect(),
                    None,
                    message.clone(),
                    message,
                )
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_step_is_reported_as_what_it_was() {
        let failure = |kind, step: &str| {
            step_failure(ComponentError::Failed {
                kind,
                step: step.to_owned(),
                message: "no".to_owned(),
            })
        };
        assert_eq!(
            failure(
                StepKind::Call,
                "org.gnome.Shell.Extensions.EnableExtension(\"x\")"
            )
            .step(),
            Some(&FailedStep::DBus {
                call: "org.gnome.Shell.Extensions.EnableExtension(\"x\")".into()
            })
        );
        assert_eq!(
            failure(StepKind::Setting, "org.gnome.shell enabled-extensions").step(),
            Some(&FailedStep::Setting {
                key: "org.gnome.shell enabled-extensions".into()
            })
        );
        assert_eq!(
            failure(StepKind::Command, "im-config -w ibus").step(),
            Some(&FailedStep::Command {
                executable: "im-config".into(),
                arguments: vec!["-w".into(), "ibus".into()],
                exit_status: None,
                stderr: "no".into(),
            })
        );
        assert_eq!(
            step_failure(ComponentError::Cancelled),
            SystemConfiguratorError::Cancelled
        );
    }

    #[test]
    fn only_a_status_that_is_ready_at_once_is_read() {
        assert_eq!(immediately(async { 7 }), Some(7));
        assert_eq!(immediately(std::future::pending::<u8>()), None);
    }

    #[test]
    fn a_desktop_myna_does_not_know_needs_nothing_from_it() {
        let platform = Platform::for_profile(Profile::Generic);
        assert_eq!(platform.profile(), Profile::Generic);
        assert!(platform.activation().is_none());
        assert!(platform.components().required().is_empty());
        assert_eq!(platform.diagnostics(), DesktopDiagnostics::default());
    }

    #[test]
    fn a_dark_theme_darkens_settings_except_where_gnome_drives_it() {
        use gtk4 as gtk;
        crate::ui::on_gtk_thread(|| {
            gtk::init().expect("display");
            adw::init().expect("libadwaita");
            let settings = gtk::Settings::default().expect("display");
            let scheme = |profile| Platform::for_profile(profile).color_scheme();
            settings.set_gtk_theme_name(Some("Greybird-dark"));
            assert_eq!(scheme(Profile::Generic), adw::ColorScheme::ForceDark);
            assert_eq!(scheme(Profile::Gnome), adw::ColorScheme::Default);
            settings.set_gtk_theme_name(Some("Greybird"));
            assert_eq!(scheme(Profile::Generic), adw::ColorScheme::Default);
        });
    }

    #[test]
    fn a_profile_named_by_nothing_known_is_the_generic_one() {
        let env = SessionEnv::from_vars(|name| (name == "MYNA_PLATFORM").then(|| "kde".to_owned()));
        assert_eq!(Platform::select(&env).profile(), Profile::Generic);
        let env =
            SessionEnv::from_vars(|name| (name == "XDG_CURRENT_DESKTOP").then(|| "KDE".to_owned()));
        assert_eq!(Platform::select(&env).profile(), Profile::Generic);
    }
}
