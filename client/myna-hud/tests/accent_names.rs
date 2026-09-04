// tests/accent_names.rs — the lab's accent-option enumeration (feature 004).
// The lab derives its accent list from libadwaita's OWN AdwAccentColor enum
// rather than a hardcoded table, so the names always match the running
// libadwaita. This pins the enumeration: the nicks the lab will present.

use gtk::glib::EnumClass;
use gtk4 as gtk;
use libadwaita::AccentColor;

#[test]
fn the_accent_enum_yields_the_expected_names() {
    let class = EnumClass::new::<AccentColor>();
    let nicks: Vec<&str> = class.values().iter().map(|v| v.nick()).collect();
    for expected in [
        "blue", "teal", "green", "yellow", "orange", "red", "pink", "purple", "slate",
    ] {
        assert!(
            nicks.contains(&expected),
            "the enum exposes {expected} (got {nicks:?})"
        );
    }
}

#[test]
fn the_enum_has_no_duplicate_nicks() {
    let class = EnumClass::new::<AccentColor>();
    let nicks: Vec<&str> = class.values().iter().map(|v| v.nick()).collect();
    let unique: std::collections::HashSet<&str> = nicks.iter().copied().collect();
    assert_eq!(
        nicks.len(),
        unique.len(),
        "accents are distinct (got {nicks:?})"
    );
}
