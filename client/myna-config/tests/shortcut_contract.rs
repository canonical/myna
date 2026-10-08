use myna_config::shortcut::{default_key, DefaultKey, ShortcutState};

#[test]
fn setup_sets_the_default_key_only_when_none_is_bound() {
    let unbound = ShortcutState::Unbound;
    assert_eq!(default_key(&unbound, true), DefaultKey::Install);
    assert_eq!(default_key(&unbound, false), DefaultKey::Leave);
    assert_eq!(
        default_key(&ShortcutState::Bound("<Control><Alt>d".to_owned()), true),
        DefaultKey::Leave
    );
}

#[test]
fn setup_waits_for_the_daemon() {
    assert_eq!(
        default_key(&ShortcutState::NotRunning, true),
        DefaultKey::Wait
    );
}

#[test]
fn a_desktop_binding_is_the_shortcut() {
    assert_eq!(
        ShortcutState::observe(true, Some("<Super>j")),
        ShortcutState::Bound("<Super>j".into())
    );
    assert_eq!(
        ShortcutState::observe(true, Some("")),
        ShortcutState::Unbound
    );
    assert_eq!(ShortcutState::observe(true, None), ShortcutState::Unbound);
    assert_eq!(
        ShortcutState::observe(false, Some("<Super>j")),
        ShortcutState::NotRunning
    );
}

#[test]
fn accelerators_match_across_gnome_spellings() {
    use myna_platform::activation::Accelerator;
    let same_accelerator = |a: &str, b: &str| Accelerator::parse(a).is_ok_and(|a| a.same_keys(b));
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
    use myna_platform::activation::Accelerator;
    let same_accelerator = |a: &str, b: &str| Accelerator::parse(a).is_ok_and(|a| a.same_keys(b));
    assert!(!same_accelerator("<Super>l", "<Super><Shift>l"));
    assert!(!same_accelerator("<Super>l", "<Super>k"));
    assert!(!same_accelerator("", "<Super>l"));
    assert!(!same_accelerator("", ""));
}
