// tests/accent.rs — hermetic contract test for the pure accent-color
// resolution (feature 004, 2026-07-30 wave-ribbon redesign; contract
// extension.md RC25; mechanism amended R26). The reduced-motion half (RC26)
// is covered by tests/motion.rs — the resolution moved there with E2b's
// two-safe-sources design.

use myna_hud::accent::{
    derive_palette, fallback_palette, resolve_theme_accent_palette, UBUNTU_AUBERGINE, UBUNTU_ORANGE,
};
use myna_hud::shader::Rgb;

// --- The last-resort colour ---------------------------------------------
// Reached only when the desktop reports no accent at all. The `accent-color`
// NAME table that used to sit in front of this is gone (2026-08-26): it
// existed to map a settings name onto a colour ourselves, which the theme
// already does — and does better, covering Yaru variants and `wartybrown`,
// which upstream has no enum member for.

#[test]
fn the_fallback_is_ubuntu_orange_with_aubergine() {
    let fallback = fallback_palette();
    assert_eq!(
        fallback.main.to_ascii_lowercase(),
        UBUNTU_ORANGE.to_ascii_lowercase()
    );
    assert_eq!(
        fallback.darker_complement.to_ascii_lowercase(),
        UBUNTU_AUBERGINE.to_ascii_lowercase(),
        "the orange fallback keeps the fixed aubergine secondary"
    );
}

#[test]
fn derive_palette_shape() {
    let palette = derive_palette("#0073e5", false);
    assert_ne!(
        palette.highlight, palette.main,
        "highlight tone differs from main (lighter)"
    );
    assert!(
        (0.0..=1.0).contains(&palette.translucent_alpha),
        "translucentAlpha is a valid alpha"
    );
}

// --- R26: the accent as the THEME reports it ----------------------------
// The primary path: whatever colour the desktop is actually using becomes
// the ribbon's main tone, with no question asked about whether the user
// "chose" it.

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
