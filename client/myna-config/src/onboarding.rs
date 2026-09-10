//! What a machine is missing before dictation works, and the three-step flow
//! that fixes it.
//!
//! GTK-independent so it can be exercised headlessly. The rules here decide
//! *what* is missing and *who* can fix it; the strings the user reads and the
//! snapd calls that install anything live in `onboarding_ui` and
//! `adapters::snapd_client` respectively.

use std::path::{Path, PathBuf};

use crate::diagnostics::InstalledSnap;

/// The client snap.
pub const MYNA_SNAP: &str = "myna";

/// The recommended backend, and the model component its snap is useless
/// without: the component is `type: standard` rather than default, so an
/// install that omits it leaves a backend with no weights.
pub const RECOMMENDED_BACKEND_SNAP: &str = "myna-parakeet";
pub const RECOMMENDED_MODEL_COMPONENT: &str = "model-parakeet-int8";

/// Snap plus model component as installed, rounded to the nearest 10 MB.
pub const RECOMMENDED_MODEL_MEGABYTES: u32 = 690;

/// The GNOME Shell extension that hosts the HUD.
pub const SHELL_EXTENSION_UUID: &str = "myna-shell@canonical.com";

/// One thing onboarding checks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentId {
    /// The dictation client itself.
    Myna,
    /// A speech-to-text backend with its model.
    Model,
    /// The GNOME Shell extension hosting the HUD.
    ShellExtension,
}

/// A snap this application is allowed to ask snapd to install. Typed rather
/// than a name, so no string from anywhere else can reach the install call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallTarget {
    RecommendedModel,
}

impl InstallTarget {
    pub const fn snap(self) -> &'static str {
        match self {
            Self::RecommendedModel => RECOMMENDED_BACKEND_SNAP,
        }
    }

    pub const fn components(self) -> &'static [&'static str] {
        match self {
            Self::RecommendedModel => &[RECOMMENDED_MODEL_COMPONENT],
        }
    }
}

/// What the application can do about a component that is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// snapd installs it, subject to polkit.
    Install(InstallTarget),
    /// Only the user can install it; the application explains how.
    ///
    /// Myna is here rather than behind an Install button because snapd refuses
    /// to install a snap declaring a user daemon unless
    /// `experimental.user-daemons` is set or the snap-id is allowlisted
    /// upstream - a button offering it would fail on every stock machine. The
    /// shell extension is here because it is not published anywhere snapd can
    /// reach.
    Explain,
}

/// One assessed component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub id: ComponentId,
    /// Whether onboarding may finish without it.
    pub required: bool,
    pub satisfied: bool,
    pub remedy: Remedy,
}

/// The observations onboarding is assessed from. `backend_discovered` is
/// discovery's answer, not a name match on the snap inventory: which snaps are
/// backends is a property of the interfaces they publish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    pub myna_installed: bool,
    pub backend_discovered: bool,
    pub shell_extension_installed: bool,
}

impl Machine {
    pub fn new(
        installed_snaps: &[InstalledSnap],
        backends_discovered: usize,
        shell_extension_installed: bool,
    ) -> Self {
        Self {
            myna_installed: installed_snaps.iter().any(|snap| snap.name == MYNA_SNAP),
            backend_discovered: backends_discovered > 0,
            shell_extension_installed,
        }
    }
}

/// Assess every component, in the order the wizard lists them.
///
/// The extension is not required: dictation works without it (the daemon falls
/// back to desktop notifications), and gating the flow on a manual
/// `gnome-extensions` copy would strand every user who cannot perform it.
pub fn assess(machine: Machine) -> Vec<Component> {
    vec![
        Component {
            id: ComponentId::Myna,
            required: true,
            satisfied: machine.myna_installed,
            remedy: Remedy::Explain,
        },
        Component {
            id: ComponentId::Model,
            required: true,
            satisfied: machine.backend_discovered,
            remedy: Remedy::Install(InstallTarget::RecommendedModel),
        },
        Component {
            id: ComponentId::ShellExtension,
            required: false,
            satisfied: machine.shell_extension_installed,
            remedy: Remedy::Explain,
        },
    ]
}

/// Whether the wizard should open at all.
pub fn needs_onboarding(components: &[Component]) -> bool {
    components
        .iter()
        .any(|component| component.required && !component.satisfied)
}

/// A component the wizard should not list because there is nothing to do about
/// it: a satisfied component whose row would only be reassurance.
pub fn outstanding(components: &[Component]) -> Vec<Component> {
    components
        .iter()
        .copied()
        .filter(|component| !component.satisfied)
        .collect()
}

/// The wizard's steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Welcome,
    Components,
    Shortcut,
}

impl Step {
    pub const fn first() -> Self {
        Self::Welcome
    }

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Welcome => Some(Self::Components),
            Self::Components => Some(Self::Shortcut),
            Self::Shortcut => None,
        }
    }

    pub const fn previous(self) -> Option<Self> {
        match self {
            Self::Welcome => None,
            Self::Components => Some(Self::Welcome),
            Self::Shortcut => Some(Self::Components),
        }
    }
}

/// Whether the step's forward button is sensitive. Only the component step
/// gates: every required component must be satisfied before the flow can
/// claim dictation is set up.
pub fn can_advance(step: Step, components: &[Component]) -> bool {
    match step {
        Step::Welcome | Step::Shortcut => true,
        Step::Components => !needs_onboarding(components),
    }
}

/// Where a GNOME Shell extension with [`SHELL_EXTENSION_UUID`] would be
/// installed, per-user first.
pub fn shell_extension_directories(data_home: &Path, system_data_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    for directory in std::iter::once(data_home).chain(system_data_dirs.iter().map(PathBuf::as_path))
    {
        let candidate = extension_directory(directory);
        if !directories.contains(&candidate) {
            directories.push(candidate);
        }
    }
    directories
}

fn extension_directory(data_directory: &Path) -> PathBuf {
    data_directory
        .join("gnome-shell")
        .join("extensions")
        .join(SHELL_EXTENSION_UUID)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(name: &str) -> InstalledSnap {
        InstalledSnap {
            name: name.to_owned(),
            version: "1".to_owned(),
        }
    }

    #[test]
    fn a_bare_machine_needs_onboarding() {
        let components = assess(Machine::new(&[], 0, false));
        assert!(needs_onboarding(&components));
        assert_eq!(outstanding(&components).len(), 3);
    }

    #[test]
    fn a_missing_shell_extension_alone_does_not_open_the_wizard() {
        let machine = Machine::new(&[snap("myna")], 1, false);
        let components = assess(machine);
        assert!(!needs_onboarding(&components));
        assert_eq!(
            outstanding(&components)
                .iter()
                .map(|component| component.id)
                .collect::<Vec<_>>(),
            vec![ComponentId::ShellExtension]
        );
    }

    #[test]
    fn an_installed_backend_snap_is_not_a_discovered_backend() {
        // The snap being on disk says nothing about it publishing the socket
        // interface; discovery is the only source for that.
        let machine = Machine::new(&[snap("myna"), snap("myna-parakeet")], 0, true);
        assert!(!machine.backend_discovered);
        assert!(needs_onboarding(&assess(machine)));
    }

    #[test]
    fn only_the_model_is_installable_by_the_application() {
        let installable: Vec<ComponentId> = assess(Machine::default())
            .into_iter()
            .filter(|component| matches!(component.remedy, Remedy::Install(_)))
            .map(|component| component.id)
            .collect();
        assert_eq!(installable, vec![ComponentId::Model]);
    }

    #[test]
    fn the_component_step_gates_on_required_components_only() {
        let bare = assess(Machine::default());
        assert!(can_advance(Step::Welcome, &bare));
        assert!(!can_advance(Step::Components, &bare));

        let extension_missing = assess(Machine::new(&[snap("myna")], 1, false));
        assert!(can_advance(Step::Components, &extension_missing));
    }

    #[test]
    fn the_steps_form_one_ordered_walk() {
        let mut step = Step::first();
        let mut walked = vec![step];
        while let Some(next) = step.next() {
            assert_eq!(next.previous(), Some(step));
            step = next;
            walked.push(step);
        }
        assert_eq!(
            walked,
            vec![Step::Welcome, Step::Components, Step::Shortcut]
        );
    }

    #[test]
    fn the_install_target_carries_the_model_component() {
        let target = InstallTarget::RecommendedModel;
        assert_eq!(target.snap(), RECOMMENDED_BACKEND_SNAP);
        assert_eq!(target.components(), [RECOMMENDED_MODEL_COMPONENT]);
    }

    #[test]
    fn extension_directories_are_user_first_and_deduplicated() {
        let directories = shell_extension_directories(
            Path::new("/home/user/.local/share"),
            &[
                PathBuf::from("/usr/share"),
                PathBuf::from("/home/user/.local/share"),
            ],
        );
        assert_eq!(
            directories,
            vec![
                PathBuf::from(
                    "/home/user/.local/share/gnome-shell/extensions/myna-shell@canonical.com"
                ),
                PathBuf::from("/usr/share/gnome-shell/extensions/myna-shell@canonical.com"),
            ]
        );
    }
}
