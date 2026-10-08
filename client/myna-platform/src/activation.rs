//! Activation: the desktop shortcut that toggles dictation.
//!
//! The desktop owns the key; Myna asks it to run a command. A backend reads,
//! binds and clears Myna's one binding, and reports what else holds a key.

use std::fmt;

use crate::Subscription;

/// A key chord in GTK accelerator syntax (`<Super>j`, `<Primary><Alt>d`),
/// which GSettings and xfconf both store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Accelerator(String);

impl Accelerator {
    /// Accepts a chord of known modifiers and one key.
    pub fn parse(text: &str) -> Result<Self, ActivationError> {
        chord(text)
            .map(|_| Self(text.trim().to_owned()))
            .ok_or_else(|| ActivationError::InvalidAccelerator(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `stored`, as a desktop spells it, names the same keys. One
    /// chord has several spellings: `<Primary>` or `<Ctrl>` for `<Control>`,
    /// `<Mod1>` for `<Alt>`, `<Mod4>` for `<Super>`, a letter in either case.
    pub fn same_keys(&self, stored: &str) -> bool {
        match (chord(&self.0), chord(stored)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Accelerator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn chord(accelerator: &str) -> Option<(Vec<&'static str>, String)> {
    let mut modifiers = Vec::new();
    let mut rest = accelerator.trim();
    while let Some(tail) = rest.strip_prefix('<') {
        let (name, after) = tail.split_once('>')?;
        modifiers.push(match name.to_ascii_lowercase().as_str() {
            "primary" | "control" | "ctrl" | "ctl" => "control",
            "alt" | "mod1" => "alt",
            "super" | "mod4" => "super",
            "shift" => "shift",
            "meta" => "meta",
            "hyper" => "hyper",
            _ => return None,
        });
        rest = after;
    }
    if rest.is_empty() || rest.contains(['<', '>']) {
        return None;
    }
    modifiers.sort_unstable();
    modifiers.dedup();
    Some((modifiers, rest.to_ascii_lowercase()))
}

/// What Myna's binding runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// The label the desktop's keyboard settings show, where it shows one.
    pub name: String,
    pub command: String,
}

/// A key the desktop already gives to something other than Myna.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// What the key does now, as the desktop describes it.
    pub action: String,
    /// The desktop will not give the key up; [`Activation::release`] fails.
    pub reserved: bool,
    /// Where the backend found it, in its own terms, for `release`.
    pub holder: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ActivationError {
    #[error("{0:?} is not a key chord")]
    InvalidAccelerator(String),
    #[error("the desktop's shortcuts cannot be reached: {0}")]
    Unavailable(String),
    #[error("the desktop keeps {0} for itself")]
    Reserved(String),
    #[error("the desktop refused the change: {0}")]
    Refused(String),
}

/// Myna's toggle shortcut on one desktop.
///
/// Calls are synchronous and made from the caller's main loop; `watch`
/// callbacks arrive there too.
pub trait Activation {
    /// The accelerator Myna's binding uses; `None` when nothing is bound.
    fn binding(&self) -> Result<Option<Accelerator>, ActivationError>;

    /// What Myna's binding runs; `None` when nothing is bound.
    fn command(&self) -> Result<Option<String>, ActivationError>;

    /// Bind `accelerator` to `action`, replacing Myna's previous binding and
    /// keeping every other shortcut.
    fn bind(&self, accelerator: &Accelerator, action: &Action) -> Result<(), ActivationError>;

    /// Remove Myna's binding, keeping every other shortcut.
    fn clear(&self) -> Result<(), ActivationError>;

    /// Every shortcut other than Myna's that holds `accelerator`, reserved
    /// ones included.
    fn conflicts(&self, accelerator: &Accelerator) -> Result<Vec<Conflict>, ActivationError>;

    /// Take the conflicting key away from what holds it.
    /// `Err(Reserved)` for a reserved one.
    fn release(&self, conflict: &Conflict) -> Result<(), ActivationError>;

    /// Call `changed` whenever Myna's binding may have changed, from Myna or
    /// from the desktop's own keyboard settings.
    fn watch(&self, changed: Box<dyn Fn()>) -> Subscription;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_of_one_chord_are_the_same_keys() {
        let super_j = Accelerator::parse("<Super>j").unwrap();
        for stored in ["<Super>j", "<Mod4>J", " <super>j ", "<Super><Super>j"] {
            assert!(super_j.same_keys(stored), "{stored}");
        }
        let ctrl_alt_d = Accelerator::parse("<Control><Alt>d").unwrap();
        for stored in ["<Primary><Alt>d", "<Alt><Ctrl>D", "<Mod1><Ctl>d"] {
            assert!(ctrl_alt_d.same_keys(stored), "{stored}");
        }
        for stored in [
            "<Super>k",
            "<Super><Shift>j",
            "j",
            "",
            "<Bogus>j",
            "<Super>",
        ] {
            assert!(!super_j.same_keys(stored), "{stored}");
        }
        assert!(Accelerator::parse("<Shift><Meta><Hyper>F1")
            .unwrap()
            .same_keys("<Hyper><Meta><Shift>f1"));
    }

    #[test]
    fn only_a_well_formed_chord_parses() {
        for text in ["<Super>j", "F12", "<Primary><Shift>space"] {
            let accelerator = Accelerator::parse(text).unwrap();
            assert_eq!(accelerator.as_str(), text);
            assert_eq!(accelerator.to_string(), text);
        }
        assert_eq!(
            Accelerator::parse(" <Super>j ").unwrap().as_str(),
            "<Super>j"
        );
        for text in [
            "",
            "  ",
            "<Super>",
            "<Super",
            "<Fn>j",
            "<Super>j>",
            "<Super>j<Alt>",
        ] {
            assert_eq!(
                Accelerator::parse(text),
                Err(ActivationError::InvalidAccelerator(text.to_owned())),
                "{text:?}"
            );
        }
    }
}
