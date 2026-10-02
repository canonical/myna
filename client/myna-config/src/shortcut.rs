//! The dictation shortcut as Myna Settings sees it, without GTK.
//!
//! The portal owns the key, and the daemon republishes the portal's own
//! description of it as `Shortcut` on `com.canonical.Myna.Dictation`. Where the
//! portal has no GlobalShortcuts the daemon says `Activation` is `control`, and
//! the key is a desktop custom shortcut instead.

/// The key a desktop shortcut is installed with, the daemon's portal default.
pub const DEFAULT_ACCELERATOR: &str = "<Super>j";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutState {
    /// Nothing owns the daemon's bus name.
    NotRunning,
    /// A daemon that predates the `Shortcut` property.
    Unpublished,
    /// The daemon holds no binding.
    Unbound,
    /// The portal's description of the binding, such as `Press <Super>j`.
    Bound(String),
}

impl ShortcutState {
    /// `owned` is whether the bus name has an owner; `shortcut` is the
    /// property's value when the daemon publishes one.
    pub fn observe(owned: bool, shortcut: Option<&str>) -> Self {
        match (owned, shortcut) {
            (false, _) => Self::NotRunning,
            (true, None) => Self::Unpublished,
            (true, Some(shortcut)) if shortcut.trim().is_empty() => Self::Unbound,
            (true, Some(shortcut)) => Self::Bound(shortcut.to_owned()),
        }
    }

    /// The control path's state: `binding` is the desktop shortcut's
    /// accelerator, when one is installed.
    pub fn observe_control(owned: bool, binding: Option<&str>) -> Self {
        match (owned, binding) {
            (false, _) => Self::NotRunning,
            (true, Some(binding)) if !binding.trim().is_empty() => Self::Bound(binding.to_owned()),
            (true, _) => Self::Unbound,
        }
    }
}

/// How the key reaches the daemon, from its `Activation` property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutPath {
    /// The portal's binding. Also a daemon that has not decided yet.
    Portal,
    /// A desktop custom shortcut that pokes the control socket, where the
    /// portal has no GlobalShortcuts.
    Control,
}

impl ShortcutPath {
    pub fn from_activation(activation: Option<&str>) -> Self {
        match activation {
            Some("control") => Self::Control,
            _ => Self::Portal,
        }
    }
}

/// What finishing setup does about the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultKey {
    /// The daemon has not said yet how it is activated.
    Wait,
    /// Install [`DEFAULT_ACCELERATOR`] as the desktop shortcut.
    Install,
    /// Ask the daemon to raise the portal's dialog, the only way to grant a
    /// portal key.
    Bind,
    /// Leave the key alone: a key the user already has, or one another
    /// shortcut holds, is theirs.
    Leave,
}

/// Decide [`DefaultKey`] from the daemon's `Activation`, the observed state,
/// whether the desktop can take the default key without a conflict, and
/// whether a portal dialog is up, whoever raised it: the user's answer there
/// answers setup too, so setup never raises a second one after it.
pub fn default_key(
    activation: Option<&str>,
    state: &ShortcutState,
    available: bool,
    dialog_up: bool,
) -> DefaultKey {
    match (activation, state) {
        _ if dialog_up && activation != Some("control") => DefaultKey::Leave,
        (_, ShortcutState::NotRunning) | (None | Some(""), _) => DefaultKey::Wait,
        (Some("control"), ShortcutState::Unbound) if available => DefaultKey::Install,
        (Some("portal"), ShortcutState::Unbound) => DefaultKey::Bind,
        _ => DefaultKey::Leave,
    }
}

/// What a surface says about a portal dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogHint {
    None,
    /// The surface's own dialog is up.
    Own,
    /// A dialog is up, the daemon's or another surface's: wait for it.
    OpenElsewhere,
    /// A dialog may still be on screen with nobody waiting for its answer:
    /// an older daemon gave up on it, or the daemon exited under it.
    MaybeLeftOpen,
}

/// What a surface's shortcut button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    Nothing,
    /// Install [`DEFAULT_ACCELERATOR`] as the desktop shortcut.
    ClaimDefault,
    /// Capture a new desktop shortcut in place.
    Capture,
    /// Stop a capture in place, keeping the key there was.
    CancelCapture,
    /// Capture a new desktop shortcut in a dialog.
    CaptureDialog,
    /// Ask the daemon to raise the portal's dialog.
    Bind,
    /// Raise the portal's dialog again for a bound key.
    Rebind,
}

/// Decide [`ButtonAction`]. `inline` is a surface with room to capture in
/// place; `capturing` is one doing so now.
pub fn button_action(
    path: ShortcutPath,
    state: &ShortcutState,
    inline: bool,
    capturing: bool,
) -> ButtonAction {
    match (path, state) {
        _ if capturing => ButtonAction::CancelCapture,
        (_, ShortcutState::NotRunning) => ButtonAction::Nothing,
        (ShortcutPath::Control, _) if inline => ButtonAction::Capture,
        (ShortcutPath::Control, ShortcutState::Unbound) => ButtonAction::ClaimDefault,
        (ShortcutPath::Control, _) => ButtonAction::CaptureDialog,
        (ShortcutPath::Portal, ShortcutState::Unbound) => ButtonAction::Bind,
        (ShortcutPath::Portal, _) => ButtonAction::Rebind,
    }
}

/// A GTK accelerator (`<Super>k`) as a shortcuts-spec trigger (`LOGO+k`),
/// which the portal takes as a preferred trigger. `None` for a modifier the
/// spec cannot name.
pub fn portal_trigger(accelerator: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut rest = accelerator.trim();
    while let Some(tail) = rest.strip_prefix('<') {
        let (name, after) = tail.split_once('>')?;
        parts.push(match name.to_ascii_lowercase().as_str() {
            "primary" | "control" | "ctrl" | "ctl" => "CTRL",
            "alt" | "mod1" => "ALT",
            "super" | "mod4" => "LOGO",
            "shift" => "SHIFT",
            _ => return None,
        });
        rest = after;
    }
    if rest.is_empty() || rest.contains(['<', '>', '+']) {
        return None;
    }
    parts.push(rest);
    Some(parts.join("+"))
}

/// How the daemon answered a bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindReply {
    Answered {
        ok: bool,
        message: String,
    },
    /// The daemon left the bus before replying: restarted, or crashed.
    DaemonGone,
    Failed(String),
}

/// What the surface makes of a [`BindReply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindEnd {
    /// Bound, or answered in the dialog: the daemon's state says which.
    Done,
    /// Another bind's dialog was already up: the same wait, not an error.
    Waiting,
    /// The dialog may outlive the bind; see [`DialogHint::MaybeLeftOpen`].
    LeftOpen,
    /// Answered in the dialog without a key: GNOME's Cancel arrives as the
    /// portal's "other" response, so it cannot be told from a backend that
    /// gave up.
    Declined,
    Failed(String),
}

/// The daemon's refusal while another bind's dialog is up.
const DIALOG_ALREADY_OPEN: &str = "a shortcut dialog is already open";
/// An older daemon's reply after it stopped waiting on its dialog (120 s).
const BIND_UNANSWERED: &str = "shortcut bind unanswered";
/// ashpd's words for the portal's "cancelled" and "other" responses.
const BIND_DECLINED: [&str; 2] = [
    "shortcut bind rejected: Portal request was cancelled",
    "shortcut bind rejected: Portal request didn't succeed with no information",
];

/// Judge a bind's reply; `legacy` is a daemon that predates
/// `BindShortcutWithParent`, the only kind that gives up on its dialog.
pub fn bind_end(reply: BindReply, legacy: bool) -> BindEnd {
    match reply {
        BindReply::Answered { ok: true, .. } => BindEnd::Done,
        BindReply::Answered { message, .. } if message.starts_with(DIALOG_ALREADY_OPEN) => {
            BindEnd::Waiting
        }
        BindReply::Answered { message, .. } if legacy && message.starts_with(BIND_UNANSWERED) => {
            BindEnd::LeftOpen
        }
        BindReply::Answered { message, .. } if BIND_DECLINED.contains(&message.as_str()) => {
            BindEnd::Declined
        }
        BindReply::Answered { message, .. } | BindReply::Failed(message) => {
            BindEnd::Failed(message)
        }
        BindReply::DaemonGone => BindEnd::LeftOpen,
    }
}

/// The GTK accelerators inside a portal trigger description.
///
/// GNOME's portal wraps the accelerator in a translated sentence
/// (`Press <Super>j`), so the accelerator is the part to render as keys. Other
/// portals describe a binding however they like and yield none.
pub fn accelerators(description: &str) -> Vec<&str> {
    description
        .split_whitespace()
        .filter(|token| is_accelerator(token))
        .collect()
}

fn is_accelerator(token: &str) -> bool {
    let mut rest = token;
    let mut modifiers = 0;
    while let Some(tail) = rest.strip_prefix('<') {
        let Some((name, after)) = tail.split_once('>') else {
            return false;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        modifiers += 1;
        rest = after;
    }
    modifiers > 0 && !rest.is_empty() && !rest.contains(['<', '>'])
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

    fn answered(ok: bool, message: &str) -> BindReply {
        BindReply::Answered {
            ok,
            message: message.to_owned(),
        }
    }

    #[test]
    fn the_step_captures_in_place_where_the_row_opens_a_dialog() {
        use ShortcutPath::{Control, Portal};
        let bound = ShortcutState::Bound("<Super>j".to_owned());
        let unbound = ShortcutState::Unbound;
        assert_eq!(
            button_action(Control, &unbound, true, false),
            ButtonAction::Capture
        );
        assert_eq!(
            button_action(Control, &bound, true, false),
            ButtonAction::Capture
        );
        assert_eq!(
            button_action(Control, &bound, true, true),
            ButtonAction::CancelCapture
        );
        assert_eq!(
            button_action(Control, &unbound, false, false),
            ButtonAction::ClaimDefault
        );
        assert_eq!(
            button_action(Control, &bound, false, false),
            ButtonAction::CaptureDialog
        );
        // The portal grants keys only in its own dialog, wherever asked.
        for inline in [true, false] {
            assert_eq!(
                button_action(Portal, &unbound, inline, false),
                ButtonAction::Bind
            );
            assert_eq!(
                button_action(Portal, &bound, inline, false),
                ButtonAction::Rebind
            );
            assert_eq!(
                button_action(Portal, &ShortcutState::Unpublished, inline, false),
                ButtonAction::Rebind
            );
            assert_eq!(
                button_action(Control, &ShortcutState::NotRunning, inline, false),
                ButtonAction::Nothing
            );
        }
    }

    #[test]
    fn an_accelerator_becomes_the_portal_trigger_it_names() {
        assert_eq!(portal_trigger("<Super>k").as_deref(), Some("LOGO+k"));
        assert_eq!(
            portal_trigger("<Control><Alt>d").as_deref(),
            Some("CTRL+ALT+d")
        );
        assert_eq!(
            portal_trigger("<Primary><Shift>Return").as_deref(),
            Some("CTRL+SHIFT+Return")
        );
        assert_eq!(
            portal_trigger("<Mod4><Mod1>space").as_deref(),
            Some("LOGO+ALT+space")
        );
        assert_eq!(portal_trigger("F8").as_deref(), Some("F8"));
        assert_eq!(portal_trigger("<Hyper>k"), None);
        assert_eq!(portal_trigger("<Super>"), None);
        assert_eq!(portal_trigger(""), None);
    }

    #[test]
    fn a_bind_refused_under_another_dialog_waits() {
        let reply = answered(false, "a shortcut dialog is already open");
        assert_eq!(bind_end(reply.clone(), false), BindEnd::Waiting);
        assert_eq!(bind_end(reply, true), BindEnd::Waiting);
    }

    #[test]
    fn an_older_daemon_giving_up_leaves_its_dialog_open() {
        let reply = answered(false, "shortcut bind unanswered: no answer within 120s");
        assert_eq!(bind_end(reply.clone(), true), BindEnd::LeftOpen);
        // A current daemon never gives up, so from it this is a failure.
        assert!(matches!(bind_end(reply, false), BindEnd::Failed(_)));
    }

    #[test]
    fn a_daemon_that_left_mid_bind_leaves_its_dialog_open() {
        assert_eq!(bind_end(BindReply::DaemonGone, false), BindEnd::LeftOpen);
        assert_eq!(bind_end(BindReply::DaemonGone, true), BindEnd::LeftOpen);
    }

    #[test]
    fn a_rejected_bind_is_a_failure_with_its_detail() {
        let detail = "shortcut bind rejected: Portal request didn't succeed";
        assert_eq!(
            bind_end(answered(false, detail), false),
            BindEnd::Failed(detail.to_owned())
        );
        assert_eq!(
            bind_end(BindReply::Failed("no daemon".to_owned()), true),
            BindEnd::Failed("no daemon".to_owned())
        );
        assert_eq!(bind_end(answered(true, "bound"), false), BindEnd::Done);
    }

    #[test]
    fn a_dialog_answered_without_a_key_is_declined_not_failed() {
        for message in [
            "shortcut bind rejected: Portal request didn't succeed with no information",
            "shortcut bind rejected: Portal request was cancelled",
        ] {
            assert_eq!(bind_end(answered(false, message), false), BindEnd::Declined);
            assert_eq!(bind_end(answered(false, message), true), BindEnd::Declined);
        }
    }
}
