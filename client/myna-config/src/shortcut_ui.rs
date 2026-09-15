//! GTK binding for the dictation shortcut, shared by the onboarding step and
//! the Myna page.
//!
//! State comes from a live proxy on `com.canonical.Myna.Dictation`, so a daemon
//! starting, a key bound, or a rebind in the desktop's settings shows up
//! without a refresh.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::onboarding::MYNA_SNAP;
use crate::shortcut::{accelerators, ShortcutState};

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
/// The daemon waits up to 120 s for the portal's dialog; the call outlives it.
const BIND_TIMEOUT_MS: i32 = 150_000;

pub struct ShortcutControl {
    keys: gtk::Box,
    button: gtk::Button,
    overlay: adw::ToastOverlay,
    compact: bool,
    describe: Box<dyn Fn(&ShortcutState)>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    state: RefCell<ShortcutState>,
    busy: Cell<bool>,
}

impl ShortcutControl {
    /// Drive `keys` and `button` from the daemon's `Shortcut`. `describe` sets
    /// the surface's own text for each state. The button's handler owns the
    /// control, so it lives as long as the button.
    pub fn attach(
        keys: gtk::Box,
        button: gtk::Button,
        overlay: adw::ToastOverlay,
        compact: bool,
        describe: Box<dyn Fn(&ShortcutState)>,
    ) {
        let control = Rc::new(Self {
            keys,
            button: button.clone(),
            overlay,
            compact,
            describe,
            proxy: RefCell::new(None),
            state: RefCell::new(ShortcutState::NotRunning),
            busy: Cell::new(false),
        });
        control.render();
        button.connect_clicked({
            let control = control.clone();
            move |_| control.activate()
        });

        let weak = Rc::downgrade(&control);
        glib::spawn_future_local(async move {
            let proxy = gio::DBusProxy::for_bus_future(
                gio::BusType::Session,
                gio::DBusProxyFlags::DO_NOT_AUTO_START,
                None,
                DICTATION_BUS,
                DICTATION_PATH,
                DICTATION_BUS,
            )
            .await;
            let (Some(control), Ok(proxy)) = (weak.upgrade(), proxy) else {
                return;
            };
            proxy.connect_local("g-properties-changed", false, {
                let weak = weak.clone();
                move |_| {
                    if let Some(control) = weak.upgrade() {
                        control.refresh();
                    }
                    None
                }
            });
            proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
                if let Some(control) = weak.upgrade() {
                    control.refresh();
                }
            });
            control.proxy.replace(Some(proxy));
            control.refresh();
        });
    }

    fn refresh(&self) {
        let state = match self.proxy.borrow().as_ref() {
            None => ShortcutState::NotRunning,
            Some(proxy) => {
                let shortcut = proxy
                    .cached_property("Shortcut")
                    .and_then(|value| value.get::<String>());
                ShortcutState::observe(proxy.name_owner().is_some(), shortcut.as_deref())
            }
        };
        self.state.replace(state);
        self.render();
    }

    fn render(&self) {
        let state = self.state.borrow().clone();
        (self.describe)(&state);

        while let Some(child) = self.keys.first_child() {
            self.keys.remove(&child);
        }
        match &state {
            ShortcutState::Bound(description) => {
                self.keys.set_visible(true);
                fill_keys(&self.keys, description, self.compact);
            }
            _ => self.keys.set_visible(false),
        }

        let (label, help) = match state {
            ShortcutState::Bound(_) | ShortcutState::Unpublished => (
                gettextrs::gettext("Change Shortcut"),
                gettextrs::gettext(
                    "Open Myna in the desktop's Apps settings, where the dictation shortcut is changed.",
                ),
            ),
            ShortcutState::Unbound | ShortcutState::NotRunning => (
                gettextrs::gettext("Set Up Shortcut"),
                gettextrs::gettext(
                    "Open the desktop's dialog to confirm a keyboard shortcut for dictation.",
                ),
            ),
        };
        self.button.set_label(&label);
        self.button
            .update_property(&[gtk::accessible::Property::Description(&help)]);
        self.button
            .set_sensitive(!self.busy.get() && state != ShortcutState::NotRunning);
    }

    fn activate(self: &Rc<Self>) {
        let state = self.state.borrow().clone();
        match state {
            ShortcutState::NotRunning => {}
            ShortcutState::Unbound => self.bind(),
            ShortcutState::Bound(_) | ShortcutState::Unpublished => self.open_app_settings(),
        }
    }

    /// Ask the daemon to bind. The portal keys a binding by the caller's app
    /// id, so only the daemon can make one it will see.
    fn bind(self: &Rc<Self>) {
        let Some(proxy) = self.proxy.borrow().clone() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.render();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // Empty asks the daemon for its default trigger.
            let reply = proxy
                .call_future(
                    "BindShortcut",
                    Some(&("",).to_variant()),
                    gio::DBusCallFlags::NONE,
                    BIND_TIMEOUT_MS,
                )
                .await;
            let Some(control) = weak.upgrade() else {
                return;
            };
            control.busy.set(false);
            let failure = match reply {
                Ok(reply) => match reply.get::<(bool, String)>() {
                    Some((true, _)) => None,
                    Some((false, message)) => Some(message),
                    None => Some(format!("unexpected reply {reply}")),
                },
                Err(error) => Some(error.message().to_owned()),
            };
            if let Some(detail) = failure {
                control.overlay.add_toast(adw::Toast::new(&format!(
                    "{}: {detail}",
                    gettextrs::gettext("Could not set up the shortcut")
                )));
            }
            control.refresh();
        });
    }

    /// GNOME lists and rebinds portal shortcuts on the app's page under Apps.
    fn open_app_settings(&self) {
        let command = format!("gnome-control-center applications {MYNA_SNAP}_{MYNA_SNAP}");
        let launched =
            gio::AppInfo::create_from_commandline(&command, None, gio::AppInfoCreateFlags::NONE)
                .and_then(|app| app.launch(&[], gio::AppLaunchContext::NONE));
        if launched.is_err() {
            self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                "Could not open the desktop settings",
            )));
        }
    }
}

/// The onboarding step's sentence for `state`.
pub fn onboarding_description(state: &ShortcutState) -> String {
    match state {
        ShortcutState::Bound(_) => {
            gettextrs::gettext("You can trigger Dictation anytime by using the keyboard shortcut:")
        }
        ShortcutState::Unbound => gettextrs::gettext(
            "Set up a keyboard shortcut to trigger Dictation. The desktop asks you to confirm it.",
        ),
        ShortcutState::NotRunning => gettextrs::gettext(
            "Myna is not running yet. The shortcut can be set up once it starts.",
        ),
        ShortcutState::Unpublished => gettextrs::gettext(
            "Dictation is triggered by the keyboard shortcut you chose the first time Myna asked for one. It is listed under Myna in the desktop's Apps settings.",
        ),
    }
}

/// The Myna page row's subtitle for `state`; the keys speak for a bound one.
pub fn row_subtitle(state: &ShortcutState) -> String {
    match state {
        ShortcutState::Bound(_) => String::new(),
        ShortcutState::Unbound => gettextrs::gettext("Not set up"),
        ShortcutState::NotRunning => gettextrs::gettext("Myna is not running"),
        ShortcutState::Unpublished => {
            gettextrs::gettext("Listed under Myna in the desktop's Apps settings")
        }
    }
}

/// Key caps for the first accelerator in `description`, or the description
/// itself when it names none GTK can parse.
fn fill_keys(keys: &gtk::Box, description: &str, compact: bool) {
    let caps = accelerators(description)
        .first()
        .and_then(|accelerator| key_caps(accelerator));
    let Some(caps) = caps else {
        keys.append(&gtk::Label::new(Some(description)));
        return;
    };
    for (index, cap) in caps.iter().enumerate() {
        if index > 0 {
            keys.append(&gtk::Label::new(Some("+")));
        }
        let label = gtk::Label::new(Some(cap));
        label.add_css_class("keycap");
        if compact {
            label.add_css_class("compact");
        }
        keys.append(&label);
    }
}

/// GTK's localized names for the modifiers and key of `accelerator`. The key's
/// own label is split off first so a `+` key survives.
fn key_caps(accelerator: &str) -> Option<Vec<String>> {
    let (key, modifiers) = gtk::accelerator_parse(accelerator)?;
    let full = gtk::accelerator_get_label(key, modifiers);
    let key_label = gtk::accelerator_get_label(key, gtk::gdk::ModifierType::empty());
    let prefix = full.strip_suffix(key_label.as_str())?;
    let mut caps: Vec<String> = prefix
        .split('+')
        .filter(|modifier| !modifier.is_empty())
        .map(str::to_owned)
        .collect();
    caps.push(key_label.to_string());
    Some(caps)
}
