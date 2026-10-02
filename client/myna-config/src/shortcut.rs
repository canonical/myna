//! The dictation shortcut as Myna Settings sees it, without GTK.
//!
//! The key is a GNOME custom shortcut that calls the daemon's `Toggle` over
//! D-Bus.

/// The key setup installs.
pub const DEFAULT_ACCELERATOR: &str = "<Super>j";

/// What the shortcut runs: the daemon's `Toggle`, with no `snap run` startup
/// in the way.
pub const TOGGLE_COMMAND: &str = "gdbus call --session --dest com.canonical.Myna.Dictation \
     --object-path /com/canonical/Myna/Dictation --method com.canonical.Myna.Dictation.Toggle";

/// The command for a daemon that publishes `Shortcut`, which only daemons
/// whose `Toggle` does nothing do: the snap's app that pokes the control
/// socket.
pub fn command(legacy: bool) -> String {
    if legacy {
        format!("/snap/bin/{}.toggle", crate::onboarding::MYNA_SNAP)
    } else {
        TOGGLE_COMMAND.to_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutState {
    /// Nothing owns the daemon's bus name.
    NotRunning,
    /// No desktop shortcut is installed.
    Unbound,
    /// The desktop shortcut's accelerator, such as `<Super>j`.
    Bound(String),
}

impl ShortcutState {
    /// `owned` is whether the bus name has an owner; `binding` is the desktop
    /// shortcut's accelerator, when one is installed.
    pub fn observe(owned: bool, binding: Option<&str>) -> Self {
        match (owned, binding) {
            (false, _) => Self::NotRunning,
            (true, Some(binding)) if !binding.trim().is_empty() => Self::Bound(binding.to_owned()),
            (true, _) => Self::Unbound,
        }
    }
}

/// What finishing setup does about the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultKey {
    /// The daemon is not running yet.
    Wait,
    /// Install [`DEFAULT_ACCELERATOR`].
    Install,
    /// Leave the key alone: a key the user already has, or one another
    /// shortcut holds, is theirs.
    Leave,
}

/// Decide [`DefaultKey`] from the observed state and whether the desktop can
/// take the default key without a conflict.
pub fn default_key(state: &ShortcutState, available: bool) -> DefaultKey {
    match state {
        ShortcutState::NotRunning => DefaultKey::Wait,
        ShortcutState::Unbound if available => DefaultKey::Install,
        _ => DefaultKey::Leave,
    }
}

/// What a surface's shortcut button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    Nothing,
    /// Capture a new key in place.
    Capture,
    /// Stop a capture in place, keeping the key there was.
    CancelCapture,
}

/// Decide [`ButtonAction`]; `capturing` is a surface doing so now.
pub fn button_action(state: &ShortcutState, capturing: bool) -> ButtonAction {
    match state {
        _ if capturing => ButtonAction::CancelCapture,
        ShortcutState::NotRunning => ButtonAction::Nothing,
        _ => ButtonAction::Capture,
    }
}

/// Whether two GSettings accelerators name the same keys. GNOME spells one
/// chord several ways: `<Primary>` or `<Ctrl>` for `<Control>`, `<Mod1>` for
/// `<Alt>`, `<Mod4>` for `<Super>`, and a letter in either case.
pub fn same_accelerator(a: &str, b: &str) -> bool {
    match (chord(a), chord(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
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
    if rest.is_empty() {
        return None;
    }
    modifiers.sort_unstable();
    modifiers.dedup();
    Some((modifiers, rest.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_surface_captures_in_place() {
        let bound = ShortcutState::Bound("<Super>j".to_owned());
        assert_eq!(
            button_action(&ShortcutState::Unbound, false),
            ButtonAction::Capture
        );
        assert_eq!(button_action(&bound, false), ButtonAction::Capture);
        assert_eq!(button_action(&bound, true), ButtonAction::CancelCapture);
        assert_eq!(
            button_action(&ShortcutState::NotRunning, false),
            ButtonAction::Nothing
        );
    }

    #[test]
    fn a_daemon_that_predates_toggle_gets_the_control_socket() {
        assert_eq!(command(false), TOGGLE_COMMAND);
        assert_eq!(command(true), "/snap/bin/myna.toggle");
    }
}
