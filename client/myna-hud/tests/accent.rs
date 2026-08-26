// tests/accent.rs — hermetic contract test for the pure accent-color
// resolution (feature 004, 2026-07-30 wave-ribbon redesign; contract
// extension.md X25; mechanism amended R26), ported 1:1 from the GJS
// test/accent.test.js. The reduced-motion half of the GJS suite (X26) is
// covered by tests/motion.rs — the resolution moved there with E2b's
// two-safe-sources design.

use myna_hud::accent::{
    accent_hex, derive_palette, resolve_accent_palette, resolve_theme_accent_palette, ACCENT_NAMES,
    UBUNTU_AUBERGINE, UBUNTU_ORANGE,
};
use myna_hud::shader::Rgb;

// --- X25: null (untouched default OR schema/key-absent) → Ubuntu orange ----

#[test]
fn x25_null_user_value_falls_back_to_ubuntu_orange() {
    let fallback = resolve_accent_palette(None);
    assert_eq!(
        fallback.main, UBUNTU_ORANGE,
        "null user-value → Ubuntu-orange main"
    );
    assert_eq!(
        fallback.darker_complement, UBUNTU_AUBERGINE,
        "the fallback itself counts as \"orange\" → aubergine complement"
    );
}

#[test]
fn x25_unrecognized_name_never_panics() {
    let unknown = resolve_accent_palette(Some("not-a-real-accent-name"));
    assert_eq!(unknown.main, UBUNTU_ORANGE, "falls back to Ubuntu orange");
}

// --- X25: a genuine user choice (including 'blue', same nick as default) ---

#[test]
fn x25_explicit_blue_is_not_the_fallback() {
    let blue = resolve_accent_palette(Some("blue"));
    assert_eq!(
        blue.main,
        accent_hex("blue").unwrap(),
        "explicit blue resolves to the libadwaita blue, NOT the fallback"
    );
    assert_ne!(
        blue.main,
        resolve_accent_palette(None).main,
        "distinguishable from the untouched-default fallback"
    );
}

#[test]
fn x25_every_name_resolves_to_its_own_hex() {
    for name in ACCENT_NAMES {
        let palette = resolve_accent_palette(Some(name));
        assert_eq!(
            palette.main,
            accent_hex(name).unwrap(),
            "{name} → its own hex table entry"
        );
    }
}

// --- The reinstated design-brief rule: aubergine iff orange, else a true
// --- colour-wheel complement for every other accent. ----------------------

#[test]
fn aubergine_only_for_orange() {
    let orange = resolve_accent_palette(Some("orange"));
    assert_eq!(
        orange.darker_complement, UBUNTU_AUBERGINE,
        "orange's darker-complement is the fixed Ubuntu aubergine"
    );
    for name in ACCENT_NAMES {
        if name == "orange" {
            continue;
        }
        let palette = resolve_accent_palette(Some(name));
        assert_ne!(
            palette.darker_complement, UBUNTU_AUBERGINE,
            "{name}'s darker-complement is NOT the aubergine override"
        );
    }
}

// --- derivePalette: highlight is lighter than main, translucent alpha ok --

#[test]
fn derive_palette_shape() {
    let palette = derive_palette(accent_hex("blue").unwrap(), false);
    assert_ne!(
        palette.highlight, palette.main,
        "highlight tone differs from main (lighter)"
    );
    assert!(
        (0.0..=1.0).contains(&palette.translucent_alpha),
        "translucentAlpha is a valid alpha"
    );
}

// --- R26: the platform-resolved accent path (libadwaita ≥ 1.7) ------------
// A genuine user choice + the style manager's own resolved accent RGBA
// (picks up Ubuntu's Yaru tints exactly); the untouched-default rule still
// wins over platform resolution (None → orange even when the platform
// reports its default).

#[test]
fn the_theme_accent_is_used_as_is() {
    // Yaru magenta — a tint the fixed table cannot name at all.
    let yaru_magenta = Rgb {
        r: 0.702,
        g: 0.298,
        b: 0.702,
    };
    let palette = resolve_theme_accent_palette(yaru_magenta);
    assert_eq!(
        palette.main.to_ascii_lowercase(),
        "#b34cb3",
        "the colour the theme reports becomes main, unmodified"
    );
    assert_ne!(
        palette.darker_complement.to_ascii_lowercase(),
        UBUNTU_AUBERGINE.to_ascii_lowercase(),
        "a non-orange accent gets a computed complement"
    );
}

#[test]
fn the_theme_path_asks_no_question_about_user_choice() {
    // The old rule discarded the platform colour unless the user had
    // "genuinely chosen" — which re-tinted an untouched Ubuntu desktop
    // running a Yaru variant back to plain orange. There is no such gate
    // now: whatever the theme reports is what the ribbon uses.
    let yaru_olive = Rgb {
        r: 0.294,
        g: 0.522,
        b: 0.004,
    };
    let palette = resolve_theme_accent_palette(yaru_olive);
    assert_eq!(palette.main.to_ascii_lowercase(), "#4b8501");
}

#[test]
fn theme_orange_still_gets_the_aubergine_complement() {
    // Orangeness used to be decided by the settings NAME; with no name in
    // play it is decided by the colour itself — for both Ubuntu's orange
    // and upstream libadwaita's.
    for orange in [
        Rgb {
            r: 0.914,
            g: 0.329,
            b: 0.125,
        }, // #e95420
        Rgb {
            r: 0.929,
            g: 0.357,
            b: 0.0,
        }, // #ed5b00
    ] {
        let palette = resolve_theme_accent_palette(orange);
        assert_eq!(
            palette.darker_complement.to_ascii_lowercase(),
            UBUNTU_AUBERGINE.to_ascii_lowercase(),
            "orange keeps the fixed aubergine secondary"
        );
    }
}

// --- The table carries Ubuntu's patched values, not upstream's ----------
// Ubuntu patches libadwaita (debian/patches/ubuntu/accent-color-*) so that
// `adw_accent_color_to_rgba` returns Yaru's tints for every accent name
// under Yaru. The same name is therefore a different colour on the
// desktops myna ships to, and a well-meaning "sync this table with
// upstream libadwaita" would silently re-tint the ribbon on every Ubuntu
// machine. These are the Yaru values.

#[test]
fn the_table_uses_ubuntus_patched_accent_values() {
    for (name, yaru, upstream) in [
        ("blue", "#0073e5", "#3584e4"),
        ("teal", "#308280", "#2190a4"),
        ("green", "#4b8501", "#3a944a"),
        ("orange", "#e95420", "#ed5b00"),
        ("red", "#da3450", "#e62d42"),
        ("pink", "#b34cb3", "#d56199"),
        ("purple", "#7764d8", "#9141ac"),
        ("slate", "#657b69", "#6f8396"),
    ] {
        assert_eq!(
            accent_hex(name),
            Some(yaru),
            "{name} uses the Yaru value, not upstream's {upstream}"
        );
    }
    // Yellow is the one accent Yaru leaves alone.
    assert_eq!(accent_hex("yellow"), Some("#c88800"));

    // Yaru's wartybrown, which upstream libadwaita does not have at all —
    // its enum value is deliberately out of upstream's range, so it can
    // only ever reach us by name or as a resolved RGBA, never as an enum.
    assert_eq!(
        accent_hex("brown"),
        Some("#b39169"),
        "the Ubuntu-only brown accent resolves"
    );
}

#[test]
fn the_fallback_orange_and_the_untouched_default_are_one_colour() {
    // Ubuntu also patches the DEFAULT accent to orange, and Yaru's orange
    // is Ubuntu orange — so the "user chose orange" path and the "user
    // never chose" path now name the same colour rather than two that
    // merely looked alike.
    assert_eq!(
        accent_hex("orange").map(str::to_ascii_lowercase),
        Some(UBUNTU_ORANGE.to_ascii_lowercase()),
    );
}
