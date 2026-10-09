//! The dictation shortcut as an xfconf custom command.
//!
//! xfsettingsd grabs every `/commands/custom/<accelerator>` of the
//! `xfce4-keyboard-shortcuts` channel and runs its string value when the key
//! goes down, so a write is live at once. Xfce stores no label, so Myna's
//! binding is the entry whose command Myna recognises as its own
//! ([`crate::shortcut::is_toggle_command`]). xfsettingsd honours
//! `/commands/custom` only while `/commands/custom/override` is true, and
//! finds the entry by its stored spelling, so any spelling of a chord works.

use gio::glib::Variant;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError, Conflict};
use myna_platform::Subscription;

use super::xfconf::Xfconf;
use crate::shortcut::is_toggle_command;

pub const CHANNEL: &str = "xfce4-keyboard-shortcuts";
const COMMANDS: &str = "/commands";
const WINDOW_MANAGER: &str = "/xfwm4";

pub struct XfconfActivation {
    xfconf: Xfconf,
    /// Where desktop entries name the commands that hold keys; none reads
    /// them raw.
    applications: Vec<std::path::PathBuf>,
}

impl XfconfActivation {
    /// The session bus's xfconfd.
    pub fn open() -> Result<Self, ActivationError> {
        Ok(Self::with_xfconf(Xfconf::session(CHANNEL)?)
            .with_applications(super::applications::application_dirs()))
    }

    pub fn with_xfconf(xfconf: Xfconf) -> Self {
        Self {
            xfconf,
            applications: Vec::new(),
        }
    }

    /// Name the commands that hold a key by the desktop entries in `dirs`.
    pub fn with_applications(mut self, dirs: Vec<std::path::PathBuf>) -> Self {
        self.applications = dirs;
        self
    }

    /// Where `provider`'s live shortcuts are: its custom tree once
    /// xfsettingsd has copied the defaults there, else its defaults.
    fn live(&self, provider: &str) -> Result<String, ActivationError> {
        let custom = self
            .xfconf
            .get(&format!("{provider}/custom/override"))?
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false);
        Ok(format!(
            "{provider}/{}",
            if custom { "custom" } else { "default" }
        ))
    }

    /// The entries of `base` that bind a key: a string directly under it.
    fn entries(&self, base: &str) -> Result<Vec<(String, String)>, ActivationError> {
        let prefix = format!("{base}/");
        Ok(self
            .xfconf
            .all(base)?
            .into_iter()
            .filter_map(|(property, value)| {
                let accelerator = property.strip_prefix(&prefix)?;
                (!accelerator.contains('/') && accelerator != "override")
                    .then(|| Some((accelerator.to_owned(), value.str()?.to_owned())))?
            })
            .collect())
    }

    /// Myna's entries in the custom tree: accelerator and command.
    fn ours(&self) -> Result<Vec<(String, String)>, ActivationError> {
        let mut ours = self.entries(&format!("{COMMANDS}/custom"))?;
        ours.retain(|(_, command)| is_toggle_command(command));
        Ok(ours)
    }

    /// Bindings are written to the custom tree, which xfsettingsd ignores
    /// until it has made it its own.
    fn require_custom(&self) -> Result<(), ActivationError> {
        if self.live(COMMANDS)?.ends_with("/custom") {
            Ok(())
        } else {
            Err(ActivationError::Unavailable(
                "xfsettingsd has not set up the keyboard shortcuts yet".into(),
            ))
        }
    }
}

impl Activation for XfconfActivation {
    fn binding(&self) -> Result<Option<Accelerator>, ActivationError> {
        self.ours()?
            .first()
            .map(|(accelerator, _)| Accelerator::parse(accelerator))
            .transpose()
    }

    fn command(&self) -> Result<Option<String>, ActivationError> {
        Ok(self.ours()?.into_iter().next().map(|(_, command)| command))
    }

    /// Writes the new entry before dropping the old one, so the key never
    /// goes unbound in between. Xfce has no place for `action.name`.
    fn bind(&self, accelerator: &Accelerator, action: &Action) -> Result<(), ActivationError> {
        self.require_custom()?;
        let wanted = custom(accelerator.as_str());
        let previous = self.ours()?;
        self.xfconf
            .set(&wanted, &Variant::from(action.command.as_str()))?;
        for (stale, _) in previous {
            let stale = custom(&stale);
            if stale != wanted {
                self.xfconf.reset(&stale, true)?;
            }
        }
        Ok(())
    }

    fn clear(&self) -> Result<(), ActivationError> {
        for (accelerator, _) in self.ours()? {
            self.xfconf.reset(&custom(&accelerator), true)?;
        }
        Ok(())
    }

    /// Other commands of xfsettingsd, then the window manager's own keys.
    /// Nothing is reserved: xfsettingsd and xfwm4 give up any key.
    fn conflicts(&self, accelerator: &Accelerator) -> Result<Vec<Conflict>, ActivationError> {
        let mut found = Vec::new();
        for provider in [COMMANDS, WINDOW_MANAGER] {
            let base = self.live(provider)?;
            for (held, what) in self.entries(&base)? {
                if !accelerator.same_keys(&held) || is_toggle_command(&what) {
                    continue;
                }
                found.push(Conflict {
                    action: if provider == COMMANDS {
                        super::applications::name_of(&what, &self.applications).unwrap_or(what)
                    } else {
                        window_manager_action(&what)
                    },
                    reserved: false,
                    holder: format!("{base}/{held}"),
                });
            }
        }
        Ok(found)
    }

    fn release(&self, conflict: &Conflict) -> Result<(), ActivationError> {
        if conflict.reserved {
            return Err(ActivationError::Reserved(conflict.action.clone()));
        }
        if !conflict.holder.contains("/custom/") {
            return Err(ActivationError::Refused(format!(
                "{} is a default xfsettingsd has not made editable",
                conflict.holder
            )));
        }
        self.xfconf.reset(&conflict.holder, true)
    }

    fn watch(&self, changed: Box<dyn Fn()>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn()> = changed.into();
        let subscription = self.xfconf.watch(move |property| {
            if property.starts_with("/commands/") {
                changed();
            }
        });
        Subscription::new(move || drop(subscription))
    }
}

fn custom(accelerator: &str) -> String {
    format!("{COMMANDS}/custom/{accelerator}")
}

/// xfwm4 names an action `close_window_key`; the key panel reads "close window".
fn window_manager_action(action: &str) -> String {
    action
        .strip_suffix("_key")
        .unwrap_or(action)
        .replace('_', " ")
}
