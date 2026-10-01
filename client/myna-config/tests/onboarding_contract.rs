//! The rules the wizard is built on: what opens it, how each component is
//! installed, and what it must not claim.

use myna_config::diagnostics::InstalledSnap;
use myna_config::onboarding::{
    assess, can_advance, completes, install_plan, needs_onboarding, ComponentId, ExtensionState,
    Machine, Step, MYNA_SNAP,
};

fn snap(name: &str) -> InstalledSnap {
    InstalledSnap {
        name: name.to_owned(),
        version: "1".to_owned(),
    }
}

#[test]
fn a_machine_with_no_myna_opens_the_wizard() {
    let components = assess(Machine::new(&[], 1));
    assert!(needs_onboarding(&components));
    assert!(components
        .iter()
        .any(|component| component.id == ComponentId::Myna && !component.satisfied()));
}

#[test]
fn a_ready_machine_never_opens_the_wizard() {
    let machine = Machine {
        user_daemons: true,
        ..Machine::new(&[snap(MYNA_SNAP)], 1)
    };
    assert!(!needs_onboarding(&assess(machine)));
}

/// The wizard's one button installs every missing snap; nothing is left for
/// a terminal.
#[test]
fn every_missing_snap_is_installed_by_the_button() {
    let plan = install_plan(&assess(Machine {
        user_daemons: true,
        ..Machine::default()
    }));
    assert!(plan.contains(&ComponentId::Myna));
    assert!(plan.contains(&ComponentId::Model));
}

/// The flag, both snaps, and the extension, in the order the step lists
/// them.
#[test]
fn the_wizard_assesses_the_flag_both_snaps_and_the_extension() {
    let ids: Vec<ComponentId> = assess(Machine::default())
        .iter()
        .map(|component| component.id)
        .collect();
    assert_eq!(
        ids,
        [
            ComponentId::UserDaemons,
            ComponentId::Myna,
            ComponentId::Model,
            ComponentId::ShellExtension
        ]
    );
}

/// Dictation works without the extension, falling back to notifications, so
/// a machine without it neither opens the wizard nor holds Next, and one
/// out of reach does not hold the step's move on either.
#[test]
fn the_extension_is_optional_but_completes_the_step() {
    let ready = |extension| {
        assess(Machine {
            user_daemons: true,
            extension,
            ..Machine::new(&[snap(MYNA_SNAP)], 1)
        })
    };
    let without = ready(ExtensionState::Unavailable);
    assert!(!needs_onboarding(&without));
    assert!(can_advance(Step::Components, &without));
    assert!(completes(&assess(Machine::default()), &without));
    assert!(completes(
        &assess(Machine::default()),
        &ready(ExtensionState::Enabled)
    ));
    // Enabling it when only it was missing leaves the move to Next.
    assert!(!completes(
        &ready(ExtensionState::Disabled),
        &ready(ExtensionState::Enabled)
    ));
}

#[test]
fn the_component_step_is_the_only_gate() {
    let bare = assess(Machine::default());
    assert!(can_advance(Step::Welcome, &bare));
    assert!(!can_advance(Step::Components, &bare));
    assert!(can_advance(Step::Shortcut, &bare));
}
