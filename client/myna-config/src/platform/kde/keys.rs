//! GTK accelerators as the Qt key codes kglobalaccel stores, and back.
//!
//! A Qt key is one `int`: a `Qt::Key` ORed with `Qt::KeyboardModifier`
//! bits. A printable key is its character, upper case for a letter; other
//! keys have codes of their own. Qt's Meta is the Super key.

use gtk4::gdk;
use gtk4::glib::translate::{FromGlib, IntoGlib};
use myna_platform::activation::Accelerator;

const SHIFT: i32 = 0x0200_0000;
const CONTROL: i32 = 0x0400_0000;
const ALT: i32 = 0x0800_0000;
const META: i32 = 0x1000_0000;
const MODIFIERS: i32 = SHIFT | CONTROL | ALT | META;
/// `Qt::KeyboardModifierMask`: the modifiers above and keypad and group switch.
const MODIFIER_MASK: i32 = 0xfe00_0000_u32 as i32;

/// GTK's modifier names in the order an accelerator spells them.
const MODIFIER_NAMES: [(&str, &str, i32); 4] = [
    ("control", "<Control>", CONTROL),
    ("alt", "<Alt>", ALT),
    ("shift", "<Shift>", SHIFT),
    ("super", "<Super>", META),
];

const F1: i32 = 0x0100_0030;
const F35: i32 = 0x0100_0052;

/// The keys that type nothing, by GDK name and `Qt::Key`.
const SPECIAL: [(&str, i32); 30] = [
    ("Escape", 0x0100_0000),
    ("Tab", 0x0100_0001),
    ("ISO_Left_Tab", 0x0100_0002),
    ("BackSpace", 0x0100_0003),
    ("Return", 0x0100_0004),
    ("Insert", 0x0100_0006),
    ("Delete", 0x0100_0007),
    ("Pause", 0x0100_0008),
    ("Print", 0x0100_0009),
    ("Sys_Req", 0x0100_000a),
    ("Clear", 0x0100_000b),
    ("Home", 0x0100_0010),
    ("End", 0x0100_0011),
    ("Left", 0x0100_0012),
    ("Up", 0x0100_0013),
    ("Right", 0x0100_0014),
    ("Down", 0x0100_0015),
    ("Page_Up", 0x0100_0016),
    ("Page_Down", 0x0100_0017),
    ("Menu", 0x0100_0055),
    ("Help", 0x0100_0058),
    ("XF86AudioLowerVolume", 0x0100_0070),
    ("XF86AudioMute", 0x0100_0071),
    ("XF86AudioRaiseVolume", 0x0100_0072),
    ("XF86AudioPlay", 0x0100_0080),
    ("XF86AudioStop", 0x0100_0081),
    ("XF86AudioPrev", 0x0100_0082),
    ("XF86AudioNext", 0x0100_0083),
    ("XF86AudioRecord", 0x0100_0084),
    ("XF86AudioPause", 0x0100_0085),
];

/// `accelerator` as one Qt key, `None` for a key or modifier (Meta, Hyper)
/// Qt has no code for.
pub fn to_qt(accelerator: &Accelerator) -> Option<i32> {
    let (modifiers, name) = accelerator.parts();
    let mut code = 0;
    for modifier in modifiers {
        code |= MODIFIER_NAMES
            .iter()
            .find(|(gtk, _, _)| *gtk == modifier)?
            .2;
    }
    // GDK's own name for the key (`Prior` is `Page_Up`), else the spelling,
    // which `same_keys` reads in either case (`f4`).
    let canonical = gdk::Key::from_name(name).and_then(|key| key.to_lower().name());
    Some(code | key_code(canonical.as_deref().unwrap_or(name))?)
}

fn key_code(name: &str) -> Option<i32> {
    // GDK names the media keys without their `XF86`.
    let bare = |name: &'static str| name.strip_prefix("XF86").unwrap_or(name);
    if let Some((_, code)) = SPECIAL
        .iter()
        .find(|(special, _)| special.eq_ignore_ascii_case(name) || bare(special) == name)
    {
        return Some(*code);
    }
    if let Some(n) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<i32>().ok())
    {
        let code = F1 + n - 1;
        return (n >= 1 && code <= F35).then_some(code);
    }
    let key = gdk::Key::from_name(name)?;
    // Keypad, dead and other function keysyms carry no character of their own.
    if (0xfd00..=0xffff).contains(&key.into_glib()) {
        return None;
    }
    let character = key.to_unicode().filter(|c| !c.is_control())?;
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(upper), None) => Some(upper as i32),
        _ => None,
    }
}

/// The GTK accelerator for a Qt key, `None` for one GTK cannot name.
pub fn from_qt(code: i32) -> Option<Accelerator> {
    if code & MODIFIER_MASK & !MODIFIERS != 0 {
        return None;
    }
    let key = code & !MODIFIER_MASK;
    let name = if let Some((name, _)) = SPECIAL.iter().find(|(_, special)| *special == key) {
        (*name).to_owned()
    } else if (F1..=F35).contains(&key) {
        format!("F{}", key - F1 + 1)
    } else {
        let character = char::from_u32(u32::try_from(key).ok()?)?;
        let mut lower = character.to_lowercase();
        let lower = match (lower.next(), lower.next()) {
            (Some(lower), None) => lower,
            _ => return None,
        };
        keysym(lower).name()?.to_string()
    };
    let modifiers: String = MODIFIER_NAMES
        .iter()
        .filter(|(_, _, bit)| code & bit != 0)
        .map(|(_, spelled, _)| *spelled)
        .collect();
    Accelerator::parse(&format!("{modifiers}{name}")).ok()
}

/// The keysym of a character: Latin-1's is its code point, any other's the
/// code point in the Unicode keysym range. (`gdk_unicode_to_keyval` would
/// pick the legacy keysyms too, but needs GTK set up on the main thread.)
fn keysym(character: char) -> gdk::Key {
    let code = u32::from(character);
    let keysym = if code <= 0xff {
        code
    } else {
        0x0100_0000 | code
    };
    unsafe { gdk::Key::from_glib(keysym) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qt(text: &str) -> Option<i32> {
        to_qt(&Accelerator::parse(text).unwrap())
    }

    #[test]
    fn chords_take_the_codes_kglobalaccel_stores() {
        // Read from a Plasma 6.6 session's kglobalaccel.
        assert_eq!(qt("<Super>w"), Some(268_435_543));
        assert_eq!(qt("<Alt>F4"), Some(150_994_995));
        assert_eq!(qt("<Super>j"), Some(META | 0x4a));
        // Every spelling of one chord is one code.
        for spelling in ["<Primary><Alt>d", "<Ctrl><Mod1>D", "<Alt><Control>d"] {
            assert_eq!(qt(spelling), Some(CONTROL | ALT | 0x44), "{spelling}");
        }
        assert_eq!(
            qt("<Shift><Super>Page_Up"),
            Some(SHIFT | META | 0x0100_0016)
        );
        assert_eq!(qt("F12"), Some(0x0100_003b));
        assert_eq!(qt("<Mod1>f4"), qt("<Alt>F4"));
        assert_eq!(qt("<Super>Prior"), qt("<Super>Page_Up"));
        assert_eq!(qt("<Super>space"), Some(META | 0x20));
        assert_eq!(qt("<Super>comma"), Some(META | 0x2c));
        assert_eq!(qt("XF86AudioMute"), Some(0x0100_0071));
        assert_eq!(qt("<Super>eacute"), Some(META | 0xc9));
        assert_eq!(qt("<Super>Cyrillic_ya"), Some(META | 0x42f));
    }

    #[test]
    fn what_qt_has_no_code_for_is_none() {
        for text in [
            "<Hyper>j",
            "<Meta>j",
            "<Super>KP_1",
            "<Super>nosuchkey",
            "F36",
            "<Super>dead_acute",
        ] {
            assert_eq!(qt(text), None, "{text}");
        }
    }

    #[test]
    fn codes_read_back_as_the_accelerators_they_came_from() {
        for text in [
            "<Super>j",
            "<Control><Alt>d",
            "<Shift><Super>Page_Up",
            "F1",
            "F35",
            "<Alt>F4",
            "<Super>space",
            "<Super>comma",
            "XF86AudioPlay",
            "<Control><Alt><Shift><Super>Return",
            "<Super>eacute",
        ] {
            let accelerator = Accelerator::parse(text).unwrap();
            let back = from_qt(to_qt(&accelerator).unwrap()).unwrap();
            assert_eq!(back.as_str(), text);
        }
    }

    #[test]
    fn codes_gtk_cannot_name_read_back_as_none() {
        // Keypad 1, a group-switch bit, Qt's Key_Calculator, a surrogate.
        for code in [0x2000_0031, 0x4000_004a_u32 as i32, 0x0100_00cb, 0xd800] {
            assert_eq!(from_qt(code), None, "{code:#x}");
        }
    }
}
