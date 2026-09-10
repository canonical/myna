//! The rules the wizard is built on: what opens it, what it is allowed to
//! install, and what it must not claim.

use myna_config::adapters::snapd_client::InstallAction;
use myna_config::diagnostics::InstalledSnap;
use myna_config::onboarding::{
    assess, can_advance, needs_onboarding, outstanding, ComponentId, InstallTarget, Machine,
    Remedy, Step, MYNA_SNAP, RECOMMENDED_BACKEND_SNAP, RECOMMENDED_MODEL_COMPONENT,
};

fn snap(name: &str) -> InstalledSnap {
    InstalledSnap {
        name: name.to_owned(),
        version: "1".to_owned(),
    }
}

#[test]
fn a_machine_with_no_myna_opens_the_wizard() {
    let components = assess(Machine::new(&[], 0, true));
    assert!(needs_onboarding(&components));
    assert!(outstanding(&components)
        .iter()
        .any(|component| component.id == ComponentId::Myna));
}

#[test]
fn a_ready_machine_never_opens_the_wizard() {
    let machine = Machine::new(&[snap(MYNA_SNAP)], 1, true);
    assert!(!needs_onboarding(&assess(machine)));
}

/// snapd refuses to install a snap declaring a user daemon on a stock machine,
/// so Myna is explained rather than offered behind a button that cannot work.
#[test]
fn myna_itself_is_never_offered_as_an_in_app_install() {
    let myna = assess(Machine::default())
        .into_iter()
        .find(|component| component.id == ComponentId::Myna)
        .expect("the wizard assesses Myna");
    assert_eq!(myna.remedy, Remedy::Explain);
    assert!(myna.required);
}

/// The extension is not published anywhere snapd can reach, and dictation
/// works without it, so it must not block the flow.
#[test]
fn the_shell_extension_is_explained_and_does_not_gate_the_flow() {
    let extension = assess(Machine::default())
        .into_iter()
        .find(|component| component.id == ComponentId::ShellExtension)
        .expect("the wizard assesses the shell extension");
    assert_eq!(extension.remedy, Remedy::Explain);
    assert!(!extension.required);

    let only_extension_missing = assess(Machine::new(&[snap(MYNA_SNAP)], 1, false));
    assert!(can_advance(Step::Components, &only_extension_missing));
}

#[test]
fn the_component_step_is_the_only_gate() {
    let bare = assess(Machine::default());
    assert!(can_advance(Step::Welcome, &bare));
    assert!(!can_advance(Step::Components, &bare));
    assert!(can_advance(Step::Shortcut, &bare));
}

/// The one install the application performs, exactly as snapd receives it: the
/// model component is not a default component, so an install that omits it
/// leaves a backend with no weights.
#[test]
fn the_only_install_the_application_can_make_is_the_recommended_model() {
    let action = InstallAction::new(InstallTarget::RecommendedModel);
    assert_eq!(action.snap(), RECOMMENDED_BACKEND_SNAP);
    assert_eq!(action.components(), [RECOMMENDED_MODEL_COMPONENT]);
    assert_eq!(
        action.path().expect("a valid snap name"),
        format!("/v2/snaps/{RECOMMENDED_BACKEND_SNAP}")
    );
    assert_eq!(
        action.to_request_body().expect("a valid request body"),
        format!(r#"{{"action":"install","components":["{RECOMMENDED_MODEL_COMPONENT}"]}}"#)
    );
}
