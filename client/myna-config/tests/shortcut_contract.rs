use myna_config::shortcut::{
    accelerators, default_key, trigger_key, DefaultKey, ShortcutPath, ShortcutState,
};

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

/// Stands in for GTK's keyval table: the keys with no printable character.
fn named(token: &str) -> bool {
    ["F2", "F12", "Print", "XF86AudioPlay"].contains(&token)
}

#[test]
fn a_lone_named_key_is_the_trigger() {
    assert_eq!(trigger_key("Press F2", None, named), Some("F2"));
    assert_eq!(trigger_key("Appuyez sur F2.", None, named), Some("F2"));
    assert_eq!(
        trigger_key("Press XF86AudioPlay", None, named),
        Some("XF86AudioPlay")
    );
}

#[test]
fn a_modified_accelerator_wins_over_a_lone_key() {
    assert_eq!(trigger_key("Press <Super>j", None, named), Some("<Super>j"));
    assert_eq!(
        trigger_key("Print <Super>F2", None, named),
        Some("<Super>F2")
    );
}

#[test]
fn words_that_name_no_key_are_never_the_trigger() {
    assert_eq!(trigger_key("Press j", None, named), None);
    assert_eq!(trigger_key("Meta+J", None, named), None);
    assert_eq!(trigger_key("", None, named), None);
    // Two candidates: which one is the key is a guess, so neither is.
    assert_eq!(trigger_key("Print F2", None, named), None);
}

#[test]
fn the_stored_key_wins_where_the_description_names_it() {
    assert_eq!(
        trigger_key("「F2」を押します", Some("F2"), named),
        Some("F2")
    );
    assert_eq!(trigger_key("Press j", Some("j"), named), Some("j"));
    assert_eq!(
        trigger_key("<Super>j", Some("<Super>j"), named),
        Some("<Super>j")
    );
}

#[test]
fn a_stored_key_the_description_does_not_name_is_stale() {
    assert_eq!(
        trigger_key("Press <Control><Alt>k", Some("<Super>n"), named),
        Some("<Control><Alt>k")
    );
    assert_eq!(trigger_key("Press F12", Some("F1"), named), Some("F12"));
    assert_eq!(
        trigger_key("Press <Super>j", Some("j"), named),
        Some("<Super>j")
    );
    assert_eq!(
        trigger_key("Press <Super>j", Some(""), named),
        Some("<Super>j")
    );
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
