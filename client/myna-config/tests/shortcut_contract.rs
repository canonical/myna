use myna_config::shortcut::{accelerators, default_key, DefaultKey, ShortcutPath, ShortcutState};

#[test]
fn setup_sets_the_default_key_only_when_none_is_bound() {
    let unbound = ShortcutState::Unbound;
    assert_eq!(
        default_key(Some("control"), &unbound, true, false),
        DefaultKey::Install
    );
    assert_eq!(
        default_key(Some("control"), &unbound, false, false),
        DefaultKey::Leave
    );
    assert_eq!(
        default_key(
            Some("control"),
            &ShortcutState::Bound("<Control><Alt>d".to_owned()),
            true,
            false
        ),
        DefaultKey::Leave
    );
    // Only the portal's own dialog grants a key, so setup raises it.
    assert_eq!(
        default_key(Some("portal"), &unbound, true, false),
        DefaultKey::Bind
    );
    assert_eq!(
        default_key(
            Some("portal"),
            &ShortcutState::Bound("Press <Super>j".to_owned()),
            true,
            false
        ),
        DefaultKey::Leave
    );
    assert_eq!(
        default_key(Some("portal"), &ShortcutState::Unpublished, true, false),
        DefaultKey::Leave
    );
}

#[test]
fn setup_waits_for_the_daemon_to_say_how_it_is_activated() {
    assert_eq!(
        default_key(Some("control"), &ShortcutState::NotRunning, true, false),
        DefaultKey::Wait
    );
    assert_eq!(
        default_key(Some(""), &ShortcutState::Unbound, true, false),
        DefaultKey::Wait
    );
    assert_eq!(
        default_key(None, &ShortcutState::Unbound, true, false),
        DefaultKey::Wait
    );
}

#[test]
fn a_dialog_up_answers_the_default_key_bind() {
    // Whoever raised it, the user's answer there is the step's answer, so
    // finishing setup asks nothing more, decided or not.
    for activation in [None, Some(""), Some("portal")] {
        assert_eq!(
            default_key(activation, &ShortcutState::Unbound, true, true),
            DefaultKey::Leave
        );
    }
    assert_eq!(
        default_key(Some("portal"), &ShortcutState::NotRunning, true, true),
        DefaultKey::Leave
    );
    // Under control there is no portal dialog to answer for it.
    assert_eq!(
        default_key(Some("control"), &ShortcutState::Unbound, true, true),
        DefaultKey::Install
    );
}

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

#[test]
fn only_control_activation_takes_the_desktop_shortcut_path() {
    assert_eq!(
        ShortcutPath::from_activation(Some("control")),
        ShortcutPath::Control
    );
    for activation in [Some("portal"), Some(""), None] {
        assert_eq!(
            ShortcutPath::from_activation(activation),
            ShortcutPath::Portal
        );
    }
}

#[test]
fn a_desktop_binding_is_the_control_paths_shortcut() {
    assert_eq!(
        ShortcutState::observe_control(true, Some("<Super>j")),
        ShortcutState::Bound("<Super>j".into())
    );
    assert_eq!(
        ShortcutState::observe_control(true, Some("")),
        ShortcutState::Unbound
    );
    assert_eq!(
        ShortcutState::observe_control(true, None),
        ShortcutState::Unbound
    );
    assert_eq!(
        ShortcutState::observe_control(false, Some("<Super>j")),
        ShortcutState::NotRunning
    );
}

#[test]
fn accelerators_match_across_gnome_spellings() {
    use myna_config::shortcut::same_accelerator;
    assert!(same_accelerator("<Super>l", "<Super>L"));
    assert!(same_accelerator("<Primary><Alt>t", "<Alt><Control>t"));
    assert!(same_accelerator("<Mod4>o", "<Super>o"));
    assert!(same_accelerator(
        "<Ctrl><Mod1>Delete",
        "<Control><Alt>Delete"
    ));
    assert!(same_accelerator("XF86Calculator", "XF86Calculator"));
}

#[test]
fn different_chords_or_keys_do_not_match() {
    use myna_config::shortcut::same_accelerator;
    assert!(!same_accelerator("<Super>l", "<Super><Shift>l"));
    assert!(!same_accelerator("<Super>l", "<Super>k"));
    assert!(!same_accelerator("", "<Super>l"));
    assert!(!same_accelerator("", ""));
}
