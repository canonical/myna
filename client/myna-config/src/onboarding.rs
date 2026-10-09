//! What a machine is missing before dictation works, and the three-step flow
//! that fixes it.
//!
//! GTK-independent so it can be exercised headlessly. The rules here decide
//! *what* is missing, which of it the flow waits for, and when it moves on;
//! the strings the user reads live in `onboarding_ui`.

use myna_core::language::ModelFamily;
use myna_platform::components::{Blocker, ComponentStatus};

use crate::diagnostics::InstalledSnap;

/// The client snap.
pub const MYNA_SNAP: &str = "myna";

/// The recommended backend. Its install hook selects an engine, which installs
/// the matching model component, so a plain install is a working backend.
pub const RECOMMENDED_BACKEND_SNAP: &str = "myna-parakeet";

/// The GNOME Shell extension, as the myna-config deb (and Ubuntu's
/// `gnome-shell-ubuntu-extensions` on Stonking) ships it.
pub const SHELL_EXTENSION_UUID: &str = "myna-shell@canonical.com";

/// One thing onboarding checks for, in the order the component step lists
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentId {
    /// snapd's `experimental.user-daemons`, without which snapd refuses to
    /// install or refresh Myna.
    UserDaemons,
    /// The dictation client itself.
    Myna,
    /// A speech-to-text backend with its model.
    Model,
    /// The desktop's input method, where Myna needs it set up (Xfce: IBus).
    /// Not listed on a desktop that brings its own.
    InputMethod,
    /// What hosts the dictation indicator: GNOME's Shell extension, Xfce's
    /// autostarted HUD. Dictation works without it, falling back to desktop
    /// notifications.
    StatusSurface,
}

impl ComponentId {
    pub const ALL: [Self; 5] = [
        Self::UserDaemons,
        Self::Myna,
        Self::Model,
        Self::InputMethod,
        Self::StatusSurface,
    ];

    /// Whether dictation needs it. Only these gate the component step and
    /// open the wizard at startup. The desktop's pieces do not, bar an input
    /// method that is not installed (`Component::blocks`): the wizard cannot
    /// install them, and Diagnostics says what is missing.
    pub const fn required(self) -> bool {
        !self.is_desktop()
    }

    /// Whether the desktop provides it, through `myna_platform`'s
    /// `Components`, rather than snapd.
    pub const fn is_desktop(self) -> bool {
        matches!(self, Self::InputMethod | Self::StatusSurface)
    }
}

/// Where one component stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentState {
    Satisfied,
    /// Done as far as the wizard goes, and takes effect at the next login.
    AfterRelogin,
    /// Missing, and the wizard can install or turn it on.
    Missing,
    /// Missing, and out of the wizard's reach.
    Unavailable(Unavailable),
}

/// Why a component is out of the wizard's reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Nothing on this system provides it.
    NotInstalled,
    /// Hidden by a copy in the user's data dir, which gnome-shell loads
    /// first; a re-login does not help, removing that copy does.
    Shadowed,
    /// The user turned off what runs it (the Extensions app's switch).
    TurnedOff,
    /// It ran and failed.
    Failed,
    /// It does not support the running desktop, or the user chose something
    /// it cannot work with.
    Incompatible,
    /// The administrator does not let the user enable it.
    Locked,
}

impl Unavailable {
    fn of(status: ComponentStatus) -> Option<Self> {
        match status {
            ComponentStatus::Blocked(Blocker::Shadowed) => Some(Self::Shadowed),
            ComponentStatus::Blocked(Blocker::TurnedOff) => Some(Self::TurnedOff),
            ComponentStatus::Blocked(Blocker::Incompatible) => Some(Self::Incompatible),
            ComponentStatus::Blocked(Blocker::Locked) => Some(Self::Locked),
            ComponentStatus::Failed => Some(Self::Failed),
            ComponentStatus::Unavailable => Some(Self::NotInstalled),
            _ => None,
        }
    }
}

impl ComponentState {
    /// A desktop component's status as the wizard reads it: what it can turn
    /// on is missing, what is set for the next login is done.
    fn of(status: ComponentStatus) -> Self {
        match status {
            ComponentStatus::Active => Self::Satisfied,
            ComponentStatus::ActiveAfterRelogin => Self::AfterRelogin,
            ComponentStatus::Inactive | ComponentStatus::NeedsRelogin => Self::Missing,
            blocked => {
                Self::Unavailable(Unavailable::of(blocked).unwrap_or(Unavailable::NotInstalled))
            }
        }
    }
}

/// One assessed component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub id: ComponentId,
    pub state: ComponentState,
}

impl Component {
    /// Whether dictation cannot work while this stands. A desktop's pieces
    /// do not, but for an input method that is not installed: nothing can
    /// be typed without one.
    pub fn blocks(&self) -> bool {
        (self.id.required() && !self.satisfied()) || self.lacks_input_method()
    }

    fn lacks_input_method(&self) -> bool {
        self.id == ComponentId::InputMethod
            && self.state == ComponentState::Unavailable(Unavailable::NotInstalled)
    }

    pub fn satisfied(&self) -> bool {
        matches!(
            self.state,
            ComponentState::Satisfied | ComponentState::AfterRelogin
        )
    }
}

/// How gnome-shell runs an extension (`GetExtensionInfo`'s `state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionRun {
    Enabled,
    /// Disabled, or initialized and never enabled.
    Disabled,
    /// Not running because the user turned all extensions off.
    TurnedOff,
    /// Errored, uninstalled, or a state this code does not know: enabling
    /// it would not run it.
    Failed,
    /// Out of date: its `shell-version` lacks the running gnome-shell.
    OutOfDate,
    /// Not running, and the administrator locked `enabled-extensions`.
    Locked,
}

/// What gnome-shell reports for an extension it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionInfo {
    /// Installed under a system data directory rather than the user's.
    pub system: bool,
    pub run: ExtensionRun,
}

/// What the user's `org.gnome.shell` settings say of the extension: all
/// there is to go by for a copy gnome-shell has not scanned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionListing {
    /// In `enabled-extensions` and not in `disabled-extensions`.
    Enabled,
    /// Not listed, and the user may list it.
    Unlisted,
    /// Not listed, and the administrator locked the lists.
    Locked,
    /// `disable-user-extensions` holds every extension off.
    TurnedOff,
    /// No `org.gnome.shell` schema, so no gnome-shell to run it.
    Unknown,
}

/// Which copies of the extension are on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtensionCopies {
    /// Under a system data dir: the myna-config deb's copy, or Ubuntu's on
    /// Stonking.
    pub system: bool,
    /// Under the user's data dir, as the development tree installs it.
    pub user: bool,
}

/// Classify the extension. Only a system copy counts: the user copy the
/// development tree installs is not what ships. `listing` decides only for a
/// system copy gnome-shell has not scanned.
pub fn extension_state(
    reported: Option<ExtensionInfo>,
    on_disk: ExtensionCopies,
    listing: ExtensionListing,
) -> ComponentStatus {
    match reported {
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::Enabled,
        }) => ComponentStatus::Active,
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::Disabled,
        }) => ComponentStatus::Inactive,
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::TurnedOff,
        }) => ComponentStatus::Blocked(Blocker::TurnedOff),
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::Failed,
        }) => ComponentStatus::Failed,
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::OutOfDate,
        }) => ComponentStatus::Blocked(Blocker::Incompatible),
        Some(ExtensionInfo {
            system: true,
            run: ExtensionRun::Locked,
        }) => ComponentStatus::Blocked(Blocker::Locked),
        _ if on_disk.system && on_disk.user => ComponentStatus::Blocked(Blocker::Shadowed),
        _ if on_disk.system => match listing {
            ExtensionListing::Enabled => ComponentStatus::ActiveAfterRelogin,
            ExtensionListing::Unlisted => ComponentStatus::NeedsRelogin,
            ExtensionListing::Locked => ComponentStatus::Blocked(Blocker::Locked),
            ExtensionListing::TurnedOff => ComponentStatus::Blocked(Blocker::TurnedOff),
            ExtensionListing::Unknown => ComponentStatus::Unavailable,
        },
        _ => ComponentStatus::Unavailable,
    }
}

/// Which copy of the extension gnome-shell runs, named by where it lives.
/// Diagnostics shows this, never the path itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionCopy {
    /// `/usr/share/gnome/gnome-shell/extensions`, from the myna-config deb.
    MynaConfigPackage,
    /// `/usr/share/gnome-shell/extensions`, from Ubuntu's own package.
    UbuntuPackage,
    /// `/usr/share/ubuntu/gnome-shell/extensions`, a development override
    /// that shadows both packages.
    DevelopmentOverride,
    /// The user's data dir, as the development tree installs it.
    UserCopy,
    /// Any other directory.
    OtherSystemCopy,
}

impl ExtensionCopy {
    /// Classify the extension directory gnome-shell reported.
    pub fn of(path: &std::path::Path, user_data_dir: &std::path::Path) -> Self {
        let under = |dir: &std::path::Path| path.starts_with(dir.join("gnome-shell/extensions"));
        if under(user_data_dir) {
            Self::UserCopy
        } else if under(std::path::Path::new("/usr/share/gnome")) {
            Self::MynaConfigPackage
        } else if under(std::path::Path::new("/usr/share/ubuntu")) {
            Self::DevelopmentOverride
        } else if under(std::path::Path::new("/usr/share")) {
            Self::UbuntuPackage
        } else {
            Self::OtherSystemCopy
        }
    }
}

/// What Diagnostics says about the extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionReport {
    /// No gnome-shell answered.
    NoShell,
    /// gnome-shell knows no extension by this uuid.
    NotInstalled,
    /// A system copy installed since login, which gnome-shell has not
    /// scanned; `at_next_login` when the user's settings will start it then.
    SinceLogin { at_next_login: bool },
    Known {
        /// gnome-shell's own state name, such as `active` or `error`.
        state: String,
        copy: ExtensionCopy,
        /// gnome-shell's error for an extension in the error state. Error
        /// text: may name paths, redact before showing it.
        error: Option<String>,
    },
}

/// The model the wizard installs and what installing it downloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelOffer {
    pub family: ModelFamily,
    pub download_bytes: u64,
    /// The install hook may still pick the CPU engine, as it does for a GPU
    /// with no driver, and download less.
    pub upper_bound: bool,
}

impl ModelOffer {
    pub fn snap(&self) -> &'static str {
        self.family.snap_name()
    }
}

/// A download's size, or the most it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadSize {
    Exact(u64),
    UpTo(u64),
}

/// What installing Myna downloads.
pub const MYNA_DOWNLOAD_BYTES: u64 = 10_133_504;

// What each family's store install downloads on latest/edge, snap and the
// components its install hook fetches, as the store listed them on
// 2026-09-30.
const PARAKEET_SNAP_BYTES: u64 = 46_194_688;
const PARAKEET_INT8_BYTES: u64 = 729_399_296;
const PARAKEET_FP32_BYTES: u64 = 2_549_030_912;
const ONNXRUNTIME_CUDA_BYTES: u64 = 1_577_467_904;
const WHISPER_SNAP_BYTES: u64 = 121_798_656;
const WHISPER_TINY_BYTES: u64 = 76_279_808;
const WHISPER_SMALL_BYTES: u64 = 483_966_976;
const WHISPER_CUDA_BYTES: u64 = 795_668_480;
const FUNASR_SNAP_BYTES: u64 = 70_057_984;
const SENSEVOICE_BYTES: u64 = 239_083_520;

/// Parakeet, the model the wizard installs, sized for the machine.
pub fn model_offer(machine: &Machine) -> ModelOffer {
    family_offer(ModelFamily::Parakeet, machine.nvidia_gpu)
}

/// `family`, sized for the engine its install hook will pick: with an
/// NVIDIA GPU, Parakeet fetches the CUDA runtime and the fp32 model instead
/// of int8, and Whisper its CUDA runtime and the small model instead of tiny.
/// FunASR has one CPU engine.
pub fn family_offer(family: ModelFamily, nvidia_gpu: bool) -> ModelOffer {
    let (snap, cpu, gpu) = match family {
        ModelFamily::Parakeet => (
            PARAKEET_SNAP_BYTES,
            PARAKEET_INT8_BYTES,
            Some(ONNXRUNTIME_CUDA_BYTES + PARAKEET_FP32_BYTES),
        ),
        ModelFamily::Whisper => (
            WHISPER_SNAP_BYTES,
            WHISPER_TINY_BYTES,
            Some(WHISPER_CUDA_BYTES + WHISPER_SMALL_BYTES),
        ),
        ModelFamily::FunAsr => (FUNASR_SNAP_BYTES, SENSEVOICE_BYTES, None),
    };
    let gpu = gpu.filter(|_| nvidia_gpu);
    ModelOffer {
        family,
        download_bytes: snap + gpu.unwrap_or(cpu),
        upper_bound: gpu.is_some(),
    }
}

/// The observations onboarding is assessed from. `backend_discovered` is
/// discovery's answer, not a name match on the snap inventory: which snaps are
/// backends is a property of the interfaces they publish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    pub user_daemons: bool,
    pub myna_installed: bool,
    pub backend_discovered: bool,
    /// What hosts the indicator; absent where the desktop has none.
    pub status_surface: ComponentStatus,
    /// The input method, for a desktop that needs it set up; `None` where it
    /// is not Myna's to set up.
    pub input_method: Option<ComponentStatus>,
    pub nvidia_gpu: bool,
}

impl Machine {
    /// The snap observations; the rest defaults to absent.
    pub fn new(installed_snaps: &[InstalledSnap], backends_discovered: usize) -> Self {
        Self {
            myna_installed: installed_snaps.iter().any(|snap| snap.name == MYNA_SNAP),
            backend_discovered: backends_discovered > 0,
            ..Self::default()
        }
    }
}

/// Assess every component, in [`ComponentId::ALL`]'s order; the input
/// method only where the desktop needs it.
pub fn assess(machine: Machine) -> Vec<Component> {
    let installed = |satisfied: bool| {
        if satisfied {
            ComponentState::Satisfied
        } else {
            ComponentState::Missing
        }
    };
    ComponentId::ALL
        .into_iter()
        .filter_map(|id| {
            let state = match id {
                ComponentId::UserDaemons => installed(machine.user_daemons),
                ComponentId::Myna => installed(machine.myna_installed),
                ComponentId::Model => installed(machine.backend_discovered),
                ComponentId::InputMethod => ComponentState::of(machine.input_method?),
                ComponentId::StatusSurface => ComponentState::of(machine.status_surface),
            };
            Some(Component { id, state })
        })
        .collect()
}

/// The components as the step shows them while snapd installs `installing`:
/// missing until its change is done, whatever a read found half-way through
/// it. A backend's slot is published before its install has fetched the
/// model.
pub fn while_installing(components: &[Component], installing: &[ComponentId]) -> Vec<Component> {
    components
        .iter()
        .map(|component| Component {
            state: if installing.contains(&component.id) {
                ComponentState::Missing
            } else {
                component.state
            },
            ..*component
        })
        .collect()
}

/// The snap a row's Install button installs and what that is expected to
/// download; the flag and the extension are not snaps.
pub fn installs(id: ComponentId, offer: &ModelOffer) -> Option<(&'static str, u64)> {
    match id {
        ComponentId::Myna => Some((MYNA_SNAP, MYNA_DOWNLOAD_BYTES)),
        ComponentId::Model => Some((offer.snap(), offer.download_bytes)),
        ComponentId::UserDaemons | ComponentId::InputMethod | ComponentId::StatusSurface => None,
    }
}

/// Whether a required component is missing: what opens the wizard at
/// startup and holds its component step.
pub fn needs_onboarding(components: &[Component]) -> bool {
    components.iter().any(Component::blocks)
}

/// Whether the desktop's input method is not installed at all: the wizard
/// cannot install it, and without it nothing can be typed.
pub fn input_method_missing(components: &[Component]) -> bool {
    components.iter().any(Component::lacks_input_method)
}

/// What the component step's one button installs, in order: each missing
/// component the wizard can install or turn on. The flag leads, since snapd
/// refuses Myna without it; an extension out of the wizard's reach is not
/// in it, and holds nothing.
pub fn install_plan(components: &[Component]) -> Vec<ComponentId> {
    components
        .iter()
        .filter(|component| component.state == ComponentState::Missing)
        .map(|component| component.id)
        .collect()
}

/// The next step of a run that has taken `done`: a step that succeeded is
/// not retried when a read does not show it yet.
pub fn next_install(components: &[Component], done: &[ComponentId]) -> Option<ComponentId> {
    install_plan(components)
        .into_iter()
        .find(|id| !done.contains(id))
}

/// The steps of a run that one privileged set-up takes together: the flag
/// and the snaps still missing. As the user, snapd authorizes the flag, the
/// installs and the connect after them as three polkit actions, so up to
/// three prompts; the set-up asks once. Empty once only the desktop's pieces, which
/// need no authorization, are left.
pub fn set_up_steps(components: &[Component], done: &[ComponentId]) -> Vec<ComponentId> {
    if next_install(components, done).is_none_or(ComponentId::is_desktop) {
        return Vec::new();
    }
    install_plan(components)
        .into_iter()
        .filter(|id| !done.contains(id) && !id.is_desktop())
        .collect()
}

/// What the set-up of `steps` asks of snapd: the flag when it is missing,
/// each missing snap, and, with the model among them, Myna's backend plug
/// connected to it, so dictation never waits on snapd's auto-connect.
pub fn set_up_plan(steps: &[ComponentId], offer: &ModelOffer) -> crate::ports::SetUpPlan {
    crate::ports::SetUpPlan {
        flag: steps.contains(&ComponentId::UserDaemons),
        installs: steps
            .iter()
            .filter_map(|id| installs(*id, offer).map(|(snap, _)| snap))
            .collect(),
        connect: steps.contains(&ComponentId::Model).then(|| offer.snap()),
    }
}

/// The step a failed set-up's report names: the install whose `snap
/// install` failed, the model for its connect, else the flag.
pub fn failed_set_up_step(step: Option<&crate::ports::FailedStep>) -> ComponentId {
    let Some(crate::ports::FailedStep::Command { arguments, .. }) = step else {
        return ComponentId::UserDaemons;
    };
    match arguments.first().map(String::as_str) {
        Some("install") if arguments.last().map(String::as_str) == Some(MYNA_SNAP) => {
            ComponentId::Myna
        }
        Some("install" | "connect") => ComponentId::Model,
        _ => ComponentId::UserDaemons,
    }
}

/// What the button's run downloads: the snaps still missing. With the model
/// among them on an NVIDIA machine it is the most it may fetch.
pub fn remaining_download(components: &[Component], offer: &ModelOffer) -> DownloadSize {
    let plan = install_plan(components);
    let bytes = plan
        .iter()
        .filter_map(|id| installs(*id, offer))
        .map(|(_, bytes)| bytes)
        .sum();
    if offer.upper_bound && plan.contains(&ComponentId::Model) {
        DownloadSize::UpTo(bytes)
    } else {
        DownloadSize::Exact(bytes)
    }
}

/// What the component step's button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallView {
    /// Install what is missing.
    Offer,
    /// Installing or turning on this component.
    Installing(ComponentId),
    /// Nothing left that the wizard can install, yet something required is
    /// missing: only the user can fix it.
    Blocked,
    /// Nothing left that the wizard can install.
    Installed,
}

pub fn install_view(components: &[Component], installing: Option<ComponentId>) -> InstallView {
    match installing {
        Some(id) => InstallView::Installing(id),
        None if settled(components) => InstallView::Installed,
        None if install_plan(components).is_empty() => InstallView::Blocked,
        None => InstallView::Offer,
    }
}

/// Whether nothing the wizard can install is missing.
pub fn settled(components: &[Component]) -> bool {
    !needs_onboarding(components) && install_plan(components).is_empty()
}

/// Whether a desktop component waits only for the user to log out and back
/// in.
pub fn relogin_pending(components: &[Component]) -> bool {
    components.iter().any(|component| {
        component.id.is_desktop() && component.state == ComponentState::AfterRelogin
    })
}

/// Whether snapd's `experimental.user-daemons` flag is on.
pub fn flag_enabled(components: &[Component]) -> bool {
    components
        .iter()
        .any(|component| component.id == ComponentId::UserDaemons && component.satisfied())
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
}

/// Whether the step's forward button is sensitive. Only the component step
/// gates, and only on the required components: the extension is optional.
pub fn can_advance(step: Step, components: &[Component]) -> bool {
    match step {
        Step::Welcome | Step::Shortcut => true,
        Step::Components => !needs_onboarding(components),
    }
}

/// Whether the footer's Next or Done is the step's main action, styled as
/// suggested: always on the welcome, once nothing required is missing on the
/// component step, and on the last step once a key is bound, since setting
/// one up leads until then.
pub fn forward_leads(step: Step, components: &[Component], needs_key: bool) -> bool {
    match step {
        Step::Shortcut => !needs_key,
        _ => can_advance(step, components),
    }
}

/// Whether the step re-reads the machine on its own. The component step does
/// while something required is missing: the user installs in another window,
/// which the wizard may never lose focus to.
pub fn polls(step: Step, components: &[Component]) -> bool {
    step == Step::Components && needs_onboarding(components)
}

/// Whether a re-assessment found the last missing component the moment the
/// component step moves on by itself. Arriving with everything installed is
/// not one, so a re-run of the wizard does not rush past it. Nor is enabling
/// the extension when only it was missing: an instant answer, not a download
/// the user may have looked away from. An extension out of reach is skipped
/// silently, so it holds nothing. This judges re-assessments only: the
/// button's own run moves on whenever it ends with nothing left, an enabled
/// extension included, since the user asked for that run.
pub fn completes(before: &[Component], after: &[Component]) -> bool {
    let other_missing = before
        .iter()
        .any(|component| !component.id.is_desktop() && !component.satisfied());
    other_missing && settled(after)
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

    fn ready() -> Machine {
        Machine {
            user_daemons: true,
            ..Machine::new(&[snap("myna")], 1)
        }
    }

    fn with_extension(extension: ComponentStatus) -> Vec<Component> {
        assess(Machine {
            status_surface: extension,
            ..ready()
        })
    }

    #[test]
    fn a_bare_machine_needs_onboarding() {
        assert!(needs_onboarding(&assess(Machine::new(&[], 0))));
    }

    #[test]
    fn an_installed_backend_snap_is_not_a_discovered_backend() {
        // The snap being on disk says nothing about it publishing the socket
        // interface; discovery is the only source for that.
        let machine = Machine::new(&[snap("myna"), snap("myna-parakeet")], 0);
        assert!(!machine.backend_discovered);
        assert!(needs_onboarding(&assess(machine)));
    }

    #[test]
    fn every_component_is_assessed_in_the_order_the_step_lists_them() {
        let ids: Vec<ComponentId> = assess(Machine::default())
            .iter()
            .map(|component| component.id)
            .collect();
        // The input method is listed only where the desktop needs it.
        let mut all = ComponentId::ALL.to_vec();
        all.retain(|id| *id != ComponentId::InputMethod);
        assert_eq!(ids, all);
        let with_input_method = assess(Machine {
            input_method: Some(ComponentStatus::Active),
            ..Machine::default()
        });
        let ids: Vec<ComponentId> = with_input_method.iter().map(|c| c.id).collect();
        assert_eq!(ids, ComponentId::ALL);
    }

    #[test]
    fn only_the_desktops_pieces_are_optional() {
        let optional: Vec<ComponentId> = ComponentId::ALL
            .into_iter()
            .filter(|id| !id.required())
            .collect();
        assert_eq!(
            optional,
            [ComponentId::InputMethod, ComponentId::StatusSurface]
        );
    }

    #[test]
    fn an_input_method_the_wizard_can_set_up_is_run_after_the_snaps() {
        let machine = |input_method| Machine {
            input_method: Some(input_method),
            ..Machine::new(&[], 0)
        };
        let plan = install_plan(&assess(machine(ComponentStatus::NeedsRelogin)));
        assert_eq!(
            plan,
            [
                ComponentId::UserDaemons,
                ComponentId::Myna,
                ComponentId::Model,
                ComponentId::InputMethod
            ]
        );
        let components = assess(Machine {
            user_daemons: true,
            ..Machine::new(&[snap("myna")], 1)
        });
        assert!(set_up_steps(&components, &[]).is_empty());
        let components = assess(Machine {
            input_method: Some(ComponentStatus::NeedsRelogin),
            ..ready()
        });
        assert_eq!(
            next_install(&components, &[]),
            Some(ComponentId::InputMethod)
        );
        assert!(set_up_steps(&components, &[]).is_empty());
        assert_eq!(
            installs(ComponentId::InputMethod, &model_offer(&ready())),
            None
        );
    }

    #[test]
    fn what_a_desktop_piece_reports_is_what_the_step_shows() {
        for (status, state) in [
            (ComponentStatus::Active, ComponentState::Satisfied),
            (
                ComponentStatus::ActiveAfterRelogin,
                ComponentState::AfterRelogin,
            ),
            (ComponentStatus::Inactive, ComponentState::Missing),
            (ComponentStatus::NeedsRelogin, ComponentState::Missing),
            (
                ComponentStatus::Blocked(Blocker::Incompatible),
                ComponentState::Unavailable(Unavailable::Incompatible),
            ),
            (
                ComponentStatus::Failed,
                ComponentState::Unavailable(Unavailable::Failed),
            ),
            (
                ComponentStatus::Unavailable,
                ComponentState::Unavailable(Unavailable::NotInstalled),
            ),
        ] {
            let components = assess(Machine {
                input_method: Some(status),
                ..ready()
            });
            let found = components
                .iter()
                .find(|component| component.id == ComponentId::InputMethod)
                .map(|component| component.state);
            assert_eq!(found, Some(state), "{status:?}");
            // Only an IBus that is not installed holds the wizard.
            assert_eq!(
                needs_onboarding(&components),
                status == ComponentStatus::Unavailable,
                "{status:?}"
            );
        }
        let components = assess(Machine {
            input_method: Some(ComponentStatus::ActiveAfterRelogin),
            ..ready()
        });
        assert!(relogin_pending(&components));
    }

    #[test]
    fn a_missing_input_method_blocks_the_step_and_offers_nothing_to_install() {
        let components = assess(Machine {
            input_method: Some(ComponentStatus::Unavailable),
            ..ready()
        });
        assert!(needs_onboarding(&components));
        assert!(!can_advance(Step::Components, &components));
        assert!(polls(Step::Components, &components));
        assert!(install_plan(&components).is_empty());
        assert!(!settled(&components));
        assert_eq!(install_view(&components, None), InstallView::Blocked);
        assert!(input_method_missing(&components));
        assert!(!input_method_missing(&assess(ready())));
    }

    #[test]
    fn the_flag_is_required_even_with_myna_installed() {
        // An installed Myna keeps running with the flag unset, but snapd then
        // refuses its refreshes (measured on Noble).
        let machine = Machine {
            user_daemons: false,
            ..ready()
        };
        assert!(needs_onboarding(&assess(machine)));
    }

    #[test]
    fn a_missing_extension_does_not_open_the_wizard() {
        for extension in [
            ComponentStatus::Unavailable,
            ComponentStatus::Inactive,
            ComponentStatus::NeedsRelogin,
            ComponentStatus::Blocked(Blocker::Shadowed),
            ComponentStatus::Failed,
            ComponentStatus::Blocked(Blocker::Incompatible),
            ComponentStatus::Blocked(Blocker::Locked),
        ] {
            assert!(
                !needs_onboarding(&with_extension(extension)),
                "{extension:?}"
            );
        }
    }

    #[test]
    fn the_component_step_gates_on_the_required_components_only() {
        let bare = assess(Machine::default());
        assert!(can_advance(Step::Welcome, &bare));
        assert!(!can_advance(Step::Components, &bare));

        let model_missing = assess(Machine {
            backend_discovered: false,
            ..ready()
        });
        assert!(!can_advance(Step::Components, &model_missing));
        let flag_missing = assess(Machine {
            user_daemons: false,
            ..ready()
        });
        assert!(!can_advance(Step::Components, &flag_missing));
        assert!(can_advance(
            Step::Components,
            &with_extension(ComponentStatus::Unavailable)
        ));
        assert!(can_advance(
            Step::Components,
            &with_extension(ComponentStatus::Inactive)
        ));
    }

    #[test]
    fn the_forward_button_leads_only_when_moving_on_is_the_step_s_main_action() {
        let bare = assess(Machine::default());
        assert!(forward_leads(Step::Welcome, &bare, true));
        assert!(!forward_leads(Step::Components, &bare, false));
        assert!(forward_leads(Step::Components, &assess(ready()), true));
        assert!(forward_leads(Step::Shortcut, &bare, false));
        assert!(!forward_leads(Step::Shortcut, &assess(ready()), true));
    }

    #[test]
    fn the_extension_is_satisfied_only_when_enabled() {
        let state = |extension| with_extension(extension)[3].state;
        assert_eq!(state(ComponentStatus::Active), ComponentState::Satisfied);
        assert_eq!(state(ComponentStatus::Inactive), ComponentState::Missing);
        // Listing an unscanned copy is the wizard's to do; once listed, only
        // a re-login is left.
        assert_eq!(
            state(ComponentStatus::NeedsRelogin),
            ComponentState::Missing
        );
        assert_eq!(
            state(ComponentStatus::ActiveAfterRelogin),
            ComponentState::AfterRelogin
        );
        assert_eq!(
            state(ComponentStatus::Blocked(Blocker::Shadowed)),
            ComponentState::Unavailable(Unavailable::Shadowed)
        );
        assert_eq!(
            state(ComponentStatus::Blocked(Blocker::TurnedOff)),
            ComponentState::Unavailable(Unavailable::TurnedOff)
        );
        assert_eq!(
            state(ComponentStatus::Failed),
            ComponentState::Unavailable(Unavailable::Failed)
        );
        assert_eq!(
            state(ComponentStatus::Blocked(Blocker::Incompatible)),
            ComponentState::Unavailable(Unavailable::Incompatible)
        );
        assert_eq!(
            state(ComponentStatus::Blocked(Blocker::Locked)),
            ComponentState::Unavailable(Unavailable::Locked)
        );
        assert_eq!(
            state(ComponentStatus::Unavailable),
            ComponentState::Unavailable(Unavailable::NotInstalled)
        );
    }

    fn copies(system: bool, user: bool) -> ExtensionCopies {
        ExtensionCopies { system, user }
    }

    #[test]
    fn only_a_system_copy_counts() {
        let info = |system, run| Some(ExtensionInfo { system, run });
        let packaged = copies(true, false);
        let listed = ExtensionListing::Enabled;
        assert_eq!(
            extension_state(info(true, ExtensionRun::Enabled), packaged, listed),
            ComponentStatus::Active
        );
        assert_eq!(
            extension_state(info(true, ExtensionRun::Disabled), packaged, listed),
            ComponentStatus::Inactive
        );
        for (run, state) in [
            (ExtensionRun::Failed, ComponentStatus::Failed),
            (
                ExtensionRun::OutOfDate,
                ComponentStatus::Blocked(Blocker::Incompatible),
            ),
            (
                ExtensionRun::Locked,
                ComponentStatus::Blocked(Blocker::Locked),
            ),
        ] {
            assert_eq!(extension_state(info(true, run), packaged, listed), state);
        }
        assert_eq!(
            extension_state(info(true, ExtensionRun::TurnedOff), packaged, listed),
            ComponentStatus::Blocked(Blocker::TurnedOff)
        );
        // A development copy in ~/.local, enabled or not, is not what ships.
        assert_eq!(
            extension_state(
                info(false, ExtensionRun::Enabled),
                copies(false, true),
                listed
            ),
            ComponentStatus::Unavailable
        );
        assert_eq!(
            extension_state(None, ExtensionCopies::default(), listed),
            ComponentStatus::Unavailable
        );
    }

    #[test]
    fn a_system_copy_the_shell_has_not_scanned_needs_a_relogin() {
        assert_eq!(
            extension_state(None, copies(true, false), ExtensionListing::Unlisted),
            ComponentStatus::NeedsRelogin
        );
        // gnome-shell still lists a deleted user copy it scanned at login.
        assert_eq!(
            extension_state(
                Some(ExtensionInfo {
                    system: false,
                    run: ExtensionRun::Disabled
                }),
                copies(true, false),
                ExtensionListing::Unlisted
            ),
            ComponentStatus::NeedsRelogin
        );
    }

    #[test]
    fn an_unscanned_copy_follows_the_user_s_extension_settings() {
        let unscanned = |listing| extension_state(None, copies(true, false), listing);
        assert_eq!(
            unscanned(ExtensionListing::Enabled),
            ComponentStatus::ActiveAfterRelogin
        );
        assert_eq!(
            unscanned(ExtensionListing::Locked),
            ComponentStatus::Blocked(Blocker::Locked)
        );
        assert_eq!(
            unscanned(ExtensionListing::TurnedOff),
            ComponentStatus::Blocked(Blocker::TurnedOff)
        );
        // No org.gnome.shell schema: no gnome-shell to run it.
        assert_eq!(
            unscanned(ExtensionListing::Unknown),
            ComponentStatus::Unavailable
        );
        // What gnome-shell reports wins over its settings.
        assert_eq!(
            extension_state(
                Some(ExtensionInfo {
                    system: true,
                    run: ExtensionRun::Disabled
                }),
                copies(true, false),
                ExtensionListing::Enabled
            ),
            ComponentStatus::Inactive
        );
    }

    #[test]
    fn only_an_extension_listed_for_the_next_login_asks_for_a_relogin() {
        assert!(relogin_pending(&with_extension(
            ComponentStatus::ActiveAfterRelogin
        )));
        for extension in [
            ComponentStatus::Active,
            ComponentStatus::Inactive,
            ComponentStatus::NeedsRelogin,
            ComponentStatus::Unavailable,
        ] {
            assert!(
                !relogin_pending(&with_extension(extension)),
                "{extension:?}"
            );
        }
    }

    #[test]
    fn an_extension_listed_for_the_next_login_is_installed() {
        let components = with_extension(ComponentStatus::ActiveAfterRelogin);
        assert!(components[3].satisfied());
        assert!(install_plan(&components).is_empty());
        assert_eq!(install_view(&components, None), InstallView::Installed);
        assert!(settled(&components));
    }

    #[test]
    fn a_user_copy_on_disk_shadows_the_system_copy_past_a_relogin() {
        // gnome-shell loads the user data dir first and skips a uuid it has
        // already loaded, so the system copy never runs while this is there.
        for reported in [
            None,
            Some(ExtensionInfo {
                system: false,
                run: ExtensionRun::Disabled,
            }),
            Some(ExtensionInfo {
                system: false,
                run: ExtensionRun::Enabled,
            }),
        ] {
            assert_eq!(
                extension_state(reported, copies(true, true), ExtensionListing::Enabled),
                ComponentStatus::Blocked(Blocker::Shadowed),
                "{reported:?}"
            );
        }
    }

    #[test]
    fn the_button_installs_what_is_missing_flag_first() {
        let bare = assess(Machine {
            status_surface: ComponentStatus::Inactive,
            ..Machine::default()
        });
        assert_eq!(
            install_plan(&bare),
            [
                ComponentId::UserDaemons,
                ComponentId::Myna,
                ComponentId::Model,
                ComponentId::StatusSurface
            ]
        );
        let myna_only = assess(Machine {
            user_daemons: true,
            status_surface: ComponentStatus::Active,
            ..Machine::new(&[snap("myna")], 0)
        });
        assert_eq!(install_plan(&myna_only), [ComponentId::Model]);
        assert!(install_plan(&with_extension(ComponentStatus::Active)).is_empty());
    }

    #[test]
    fn an_extension_out_of_reach_is_skipped_silently() {
        for extension in [
            ComponentStatus::Unavailable,
            ComponentStatus::Blocked(Blocker::Shadowed),
            ComponentStatus::Blocked(Blocker::TurnedOff),
            ComponentStatus::Failed,
            ComponentStatus::Blocked(Blocker::Incompatible),
            ComponentStatus::Blocked(Blocker::Locked),
        ] {
            let components = with_extension(extension);
            assert!(install_plan(&components).is_empty(), "{extension:?}");
            assert_eq!(install_view(&components, None), InstallView::Installed);
        }
        for extension in [ComponentStatus::Inactive, ComponentStatus::NeedsRelogin] {
            assert_eq!(
                install_plan(&with_extension(extension)),
                [ComponentId::StatusSurface],
                "{extension:?}"
            );
        }
    }

    #[test]
    fn the_size_covers_only_the_snaps_still_missing() {
        let cpu = model_offer(&Machine::default());
        let bare = assess(Machine::default());
        assert_eq!(
            remaining_download(&bare, &cpu),
            DownloadSize::Exact(MYNA_DOWNLOAD_BYTES + cpu.download_bytes)
        );
        let myna_only = assess(Machine {
            user_daemons: true,
            ..Machine::new(&[snap("myna")], 0)
        });
        assert_eq!(
            remaining_download(&myna_only, &cpu),
            DownloadSize::Exact(cpu.download_bytes)
        );
        let model_only = assess(Machine::new(&[], 1));
        assert_eq!(
            remaining_download(&model_only, &cpu),
            DownloadSize::Exact(MYNA_DOWNLOAD_BYTES)
        );
        // The flag and the extension download nothing.
        assert_eq!(
            remaining_download(&with_extension(ComponentStatus::Inactive), &cpu),
            DownloadSize::Exact(0)
        );
        let gpu = model_offer(&Machine {
            nvidia_gpu: true,
            ..Machine::default()
        });
        assert_eq!(
            remaining_download(&bare, &gpu),
            DownloadSize::UpTo(MYNA_DOWNLOAD_BYTES + gpu.download_bytes)
        );
        // Only a missing model makes it an upper bound.
        assert_eq!(
            remaining_download(&model_only, &gpu),
            DownloadSize::Exact(MYNA_DOWNLOAD_BYTES)
        );
    }

    #[test]
    fn the_button_offers_installs_or_says_installed() {
        let bare = assess(Machine::default());
        assert_eq!(install_view(&bare, None), InstallView::Offer);
        assert_eq!(
            install_view(&bare, Some(ComponentId::Myna)),
            InstallView::Installing(ComponentId::Myna)
        );
        let complete = with_extension(ComponentStatus::Active);
        assert_eq!(install_view(&complete, None), InstallView::Installed);
        // A disabled extension is still something the button turns on.
        assert_eq!(
            install_view(&with_extension(ComponentStatus::Inactive), None),
            InstallView::Offer
        );
    }

    #[test]
    fn a_run_takes_each_missing_component_once() {
        let bare = assess(Machine::default());
        assert_eq!(next_install(&bare, &[]), Some(ComponentId::UserDaemons));
        // A step that succeeded but a read does not show yet is not retried.
        assert_eq!(
            next_install(&bare, &[ComponentId::UserDaemons]),
            Some(ComponentId::Myna)
        );
        assert_eq!(
            next_install(
                &bare,
                &[
                    ComponentId::UserDaemons,
                    ComponentId::Myna,
                    ComponentId::Model
                ]
            ),
            None
        );
        assert_eq!(
            next_install(&with_extension(ComponentStatus::Active), &[]),
            None
        );
    }

    #[test]
    fn one_set_up_takes_the_flag_and_the_missing_snaps_together() {
        let bare = assess(Machine {
            status_surface: ComponentStatus::Inactive,
            ..Machine::default()
        });
        assert_eq!(
            set_up_steps(&bare, &[]),
            [
                ComponentId::UserDaemons,
                ComponentId::Myna,
                ComponentId::Model
            ]
        );
        // The snaps still missing after the flag stay one set-up.
        assert_eq!(
            set_up_steps(&bare, &[ComponentId::UserDaemons]),
            [ComponentId::Myna, ComponentId::Model]
        );
        assert!(set_up_steps(
            &bare,
            &[
                ComponentId::UserDaemons,
                ComponentId::Myna,
                ComponentId::Model
            ]
        )
        .is_empty());
        // The flag alone, with Myna and a model installed.
        let flag_only = assess(Machine {
            myna_installed: true,
            backend_discovered: true,
            ..Machine::default()
        });
        assert_eq!(set_up_steps(&flag_only, &[]), [ComponentId::UserDaemons]);
        let flagged = assess(Machine {
            user_daemons: true,
            ..Machine::default()
        });
        assert_eq!(
            set_up_steps(&flagged, &[]),
            [ComponentId::Myna, ComponentId::Model]
        );
    }

    #[test]
    fn a_set_up_connects_the_model_it_installs() {
        let offer = model_offer(&Machine::default());
        assert_eq!(
            set_up_plan(
                &[
                    ComponentId::UserDaemons,
                    ComponentId::Myna,
                    ComponentId::Model
                ],
                &offer
            ),
            crate::ports::SetUpPlan {
                flag: true,
                installs: vec![MYNA_SNAP, RECOMMENDED_BACKEND_SNAP],
                connect: Some(RECOMMENDED_BACKEND_SNAP),
            }
        );
        // A model installed already is left to the setup after the run.
        assert_eq!(
            set_up_plan(&[ComponentId::Myna], &offer),
            crate::ports::SetUpPlan {
                flag: false,
                installs: vec![MYNA_SNAP],
                connect: None,
            }
        );
    }

    #[test]
    fn a_failed_set_up_names_the_step_whose_command_failed() {
        use crate::ports::FailedStep;
        let command = |arguments: &[&str]| FailedStep::Command {
            executable: "snap".to_owned(),
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
            exit_status: Some(1),
            stderr: String::new(),
        };
        assert_eq!(
            failed_set_up_step(Some(&command(&["install", "--edge", "myna"]))),
            ComponentId::Myna
        );
        assert_eq!(
            failed_set_up_step(Some(&command(&["install", "--edge", "myna-parakeet"]))),
            ComponentId::Model
        );
        assert_eq!(
            failed_set_up_step(Some(&command(&[
                "set",
                "system",
                "experimental.user-daemons=true"
            ]))),
            ComponentId::UserDaemons
        );
        assert_eq!(
            failed_set_up_step(Some(&command(&[
                "connect",
                "myna:backend",
                "myna-parakeet:provider"
            ]))),
            ComponentId::Model
        );
        // pkexec refusing the prompt names pkexec, before any step ran.
        assert_eq!(
            failed_set_up_step(Some(&FailedStep::Command {
                executable: "pkexec".to_owned(),
                arguments: Vec::new(),
                exit_status: Some(127),
                stderr: String::new(),
            })),
            ComponentId::UserDaemons
        );
        assert_eq!(failed_set_up_step(None), ComponentId::UserDaemons);
    }

    #[test]
    fn the_model_size_is_an_upper_bound_only_with_an_nvidia_gpu() {
        // The install hook falls back to the CPU engine when the GPU has no
        // driver, so the GPU total is the most it downloads.
        assert!(!model_offer(&Machine::default()).upper_bound);
        assert!(
            model_offer(&Machine {
                nvidia_gpu: true,
                ..Machine::default()
            })
            .upper_bound
        );
    }

    #[test]
    fn only_the_component_step_polls_and_only_while_something_required_is_missing() {
        let bare = assess(Machine::default());
        assert!(polls(Step::Components, &bare));
        assert!(!polls(
            Step::Components,
            &with_extension(ComponentStatus::Unavailable)
        ));
        assert!(!polls(Step::Welcome, &bare));
        assert!(!polls(Step::Shortcut, &bare));
    }

    #[test]
    fn only_the_last_component_appearing_moves_on_by_itself() {
        let bare = assess(Machine::default());
        let complete = with_extension(ComponentStatus::Active);
        let required_only = with_extension(ComponentStatus::Unavailable);
        let disabled = with_extension(ComponentStatus::Inactive);
        assert!(completes(&bare, &complete));
        // An extension out of reach is skipped silently: it holds nothing.
        assert!(completes(&bare, &required_only));
        // Only the extension missing is not a wait the user looked away from.
        assert!(!completes(&disabled, &complete));
        assert!(!completes(&disabled, &required_only));
        assert!(!completes(&bare, &disabled));
        assert!(!completes(&complete, &complete));
        assert!(!completes(&complete, &bare));
    }

    #[test]
    fn the_model_offer_is_parakeet_sized_for_the_engine_it_will_pick() {
        let cpu = model_offer(&Machine::default());
        assert_eq!(cpu.family, ModelFamily::Parakeet);
        assert_eq!(cpu.snap(), RECOMMENDED_BACKEND_SNAP);
        assert_eq!(cpu.download_bytes, 775_593_984);
        let gpu = model_offer(&Machine {
            nvidia_gpu: true,
            ..Machine::default()
        });
        assert_eq!(gpu.download_bytes, 4_172_693_504);
    }

    #[test]
    fn every_family_is_sized_for_the_engine_it_will_pick() {
        let sizes = |family| {
            let (cpu, gpu) = (family_offer(family, false), family_offer(family, true));
            assert_eq!((cpu.family, gpu.family), (family, family));
            (
                (cpu.download_bytes, cpu.upper_bound),
                (gpu.download_bytes, gpu.upper_bound),
            )
        };
        assert_eq!(
            sizes(ModelFamily::Parakeet),
            ((775_593_984, false), (4_172_693_504, true))
        );
        assert_eq!(
            sizes(ModelFamily::Whisper),
            ((198_078_464, false), (1_401_434_112, true))
        );
        assert_eq!(
            sizes(ModelFamily::FunAsr),
            ((309_141_504, false), (309_141_504, false)),
            "FunASR runs on the CPU alone"
        );
    }

    #[test]
    fn a_component_snapd_is_still_installing_shows_missing() {
        // A backend's slot is published before its install has fetched the
        // model, so discovery finds it half-way through the change.
        let found = with_extension(ComponentStatus::Active);
        let shown = while_installing(&found, &[ComponentId::Model]);
        assert_eq!(shown[2].state, ComponentState::Missing);
        assert!(!can_advance(Step::Components, &shown));
        assert!(!settled(&shown));
        assert_eq!(while_installing(&found, &[]), found);
        for index in [0, 1, 3] {
            assert_eq!(shown[index], found[index]);
        }
    }

    #[test]
    fn only_the_app_and_the_model_are_installed_from_snapd() {
        let offer = model_offer(&Machine::default());
        assert_eq!(
            installs(ComponentId::Myna, &offer),
            Some((MYNA_SNAP, MYNA_DOWNLOAD_BYTES))
        );
        assert_eq!(
            installs(ComponentId::Model, &offer),
            Some((RECOMMENDED_BACKEND_SNAP, offer.download_bytes))
        );
        assert_eq!(installs(ComponentId::UserDaemons, &offer), None);
        assert_eq!(installs(ComponentId::StatusSurface, &offer), None);
    }

    #[test]
    fn the_steps_form_one_ordered_walk() {
        let mut step = Step::first();
        let mut walked = vec![step];
        while let Some(next) = step.next() {
            step = next;
            walked.push(step);
        }
        assert_eq!(
            walked,
            vec![Step::Welcome, Step::Components, Step::Shortcut]
        );
    }
}
