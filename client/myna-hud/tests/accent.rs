// tests/accent.rs — hermetic contract test for the pure accent-color
// resolution (feature 004, 2026-07-30 wave-ribbon redesign; contract
// extension.md X25; mechanism amended R26), ported 1:1 from the GJS
// test/accent.test.js. The reduced-motion half of the GJS suite (X26) is
// covered by tests/motion.rs — the resolution moved there with E2b's
// two-safe-sources design.

use myna_hud::accent::{
    accent_hex, derive_palette, resolve_accent_palette, resolve_platform_accent_palette,
    AccentPalette, ACCENT_NAMES, UBUNTU_AUBERGINE, UBUNTU_ORANGE,
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
fn platform_accent_used_for_genuine_choices() {
    // The palette keeps the hex-string shape, so the platform color comes
    // back quantized to 1/255 steps — compare with that tolerance.
    fn close(a: Rgb, b: Rgb) -> bool {
        (a.r - b.r).abs() < 1.0 / 255.0
            && (a.g - b.g).abs() < 1.0 / 255.0
            && (a.b - b.b).abs() < 1.0 / 255.0
    }
    let yaru_magenta = Rgb {
        r: 0.7,
        g: 0.2,
        b: 0.6,
    };
    let palette = resolve_platform_accent_palette(Some("magenta"), yaru_magenta);
    assert!(
        close(palette.main_rgb(), yaru_magenta),
        "unknown name + platform → the platform color"
    );

    let palette = resolve_platform_accent_palette(
        Some("blue"),
        Rgb {
            r: 0.1,
            g: 0.2,
            b: 0.9,
        },
    );
    assert!(
        close(
            palette.main_rgb(),
            Rgb {
                r: 0.1,
                g: 0.2,
                b: 0.9
            }
        ),
        "known name + platform → the platform color"
    );

    assert_eq!(
        resolve_platform_accent_palette(
            None,
            Rgb {
                r: 0.1,
                g: 0.2,
                b: 0.9
            }
        )
        .main,
        UBUNTU_ORANGE,
        "untouched default → orange even when the platform reports its default"
    );
}

#[test]
fn platform_orange_keeps_the_aubergine_override() {
    let palette = resolve_platform_accent_palette(
        Some("orange"),
        Rgb {
            r: 0.93,
            g: 0.36,
            b: 0.0,
        },
    );
    assert_eq!(palette.darker_complement, UBUNTU_AUBERGINE);
}

#[test]
fn converts_to_the_ribbon_palette() {
    let palette: AccentPalette = resolve_accent_palette(Some("blue"));
    let ribbon = palette.as_ribbon_palette();
    assert_eq!(
        ribbon.main,
        myna_hud::shader::hex_to_rgb(accent_hex("blue").unwrap())
    );
    assert!((0.0..=1.0).contains(&ribbon.translucent_alpha));
}
