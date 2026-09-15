use myna_config::shortcut::{accelerators, ShortcutState};

#[test]
fn gnome_trigger_descriptions_yield_their_accelerator() {
    assert_eq!(accelerators("Press <Super>j"), vec!["<Super>j"]);
    assert_eq!(
        accelerators("Press <Control><Alt>r or <Super>F1"),
        vec!["<Control><Alt>r", "<Super>F1"]
    );
}

#[test]
fn a_translated_description_still_yields_its_accelerator() {
    assert_eq!(accelerators("Appuyez sur <Super>j"), vec!["<Super>j"]);
}

#[test]
fn descriptions_without_an_accelerator_yield_none() {
    assert!(accelerators("Meta+J").is_empty());
    assert!(accelerators("").is_empty());
    assert!(accelerators("Press <Super>").is_empty());
    assert!(accelerators("a <b> c").is_empty());
    assert!(accelerators("Press <>j").is_empty());
    assert!(accelerators("Press <Su-per>j").is_empty());
}

#[test]
fn no_owner_means_the_daemon_is_not_running() {
    assert_eq!(
        ShortcutState::observe(false, None),
        ShortcutState::NotRunning
    );
    assert_eq!(
        ShortcutState::observe(false, Some("Press <Super>j")),
        ShortcutState::NotRunning
    );
}

#[test]
fn a_daemon_without_the_property_predates_it() {
    assert_eq!(
        ShortcutState::observe(true, None),
        ShortcutState::Unpublished
    );
}

#[test]
fn an_empty_or_blank_shortcut_is_unbound() {
    assert_eq!(
        ShortcutState::observe(true, Some("")),
        ShortcutState::Unbound
    );
    assert_eq!(
        ShortcutState::observe(true, Some("  ")),
        ShortcutState::Unbound
    );
}

#[test]
fn a_published_shortcut_is_bound() {
    assert_eq!(
        ShortcutState::observe(true, Some("Press <Super>j")),
        ShortcutState::Bound("Press <Super>j".into())
    );
}
