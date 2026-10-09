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

/// Whether `command` is one Myna binds to its key: the daemon's `Toggle`, an
/// older daemon's `<snap>.toggle` app, or the unpackaged `myna-desktop
/// --toggle`. A desktop that stores no name for a shortcut is searched by it.
pub fn is_toggle_command(command: &str) -> bool {
    let command = command.trim();
    if command == TOGGLE_COMMAND {
        return true;
    }
    let mut words = command.split_whitespace();
    let (Some(program), arguments) = (words.next(), words.collect::<Vec<_>>()) else {
        return false;
    };
    let snap_app = arguments.is_empty()
        && program
            .strip_prefix("/snap/bin/")
            .and_then(|app| app.strip_suffix(".toggle"))
            .is_some_and(|snap| {
                snap == crate::onboarding::MYNA_SNAP
                    || snap.starts_with(&format!("{}_", crate::onboarding::MYNA_SNAP))
            });
    let unpackaged =
        program.rsplit('/').next() == Some("myna-desktop") && arguments == ["--toggle"];
    snap_app || unpackaged
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutState {
    /// The desktop has no shortcut Myna can manage.
    Unsupported,
    /// Nothing owns the daemon's bus name.
    NotRunning,
    /// No desktop shortcut is installed.
    Unbound,
    /// The desktop shortcut's accelerator, such as `<Super>j`.
    Bound(String),
}

impl ShortcutState {
    /// `supported` is whether the desktop has a shortcut backend; `owned` is
    /// whether the bus name has an owner; `binding` is the desktop shortcut's
    /// accelerator, when one is installed.
    pub fn observe(supported: bool, owned: bool, binding: Option<&str>) -> Self {
        match (owned, binding) {
            _ if !supported => Self::Unsupported,
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
        ShortcutState::Unsupported => DefaultKey::Leave,
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
        ShortcutState::NotRunning | ShortcutState::Unsupported => ButtonAction::Nothing,
        _ => ButtonAction::Capture,
    }
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
    fn only_myna_toggle_commands_are_recognised() {
        for ours in [
            TOGGLE_COMMAND,
            "/snap/bin/myna.toggle",
            "/snap/bin/myna_dev.toggle",
            "/usr/bin/myna-desktop --toggle",
            "myna-desktop --toggle",
        ] {
            assert!(is_toggle_command(ours), "{ours}");
        }
        for theirs in [
            "",
            "xfce4-terminal",
            "/snap/bin/other.toggle",
            "/snap/bin/myna-parakeet.toggle",
            "/snap/bin/myna.toggle --extra",
            "myna-desktop --status",
            "gdbus call --session --dest org.example.Toggle",
        ] {
            assert!(!is_toggle_command(theirs), "{theirs}");
        }
    }

    #[test]
    fn a_daemon_that_predates_toggle_gets_the_control_socket() {
        assert_eq!(command(false), TOGGLE_COMMAND);
        assert_eq!(command(true), "/snap/bin/myna.toggle");
    }
}
