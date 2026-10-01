//! GTK binding for the dictation shortcut, shared by the onboarding step and
//! the Myna page.
//!
//! State comes from a live proxy on `com.canonical.Myna.Dictation`, so a daemon
//! starting, a key bound, or a rebind in the desktop's settings shows up
//! without a refresh. Under control activation the key is the desktop custom
//! shortcut, watched the same way.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::adapters::desktop_shortcut::DesktopShortcut;
use crate::onboarding::MYNA_SNAP;
use crate::shortcut::{
    accelerators, default_key, DefaultKey, ShortcutPath, ShortcutState, DEFAULT_ACCELERATOR,
};

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
/// The daemon waits up to 120 s for the portal's dialog; the call outlives it.
const BIND_TIMEOUT_MS: i32 = 150_000;

/// Sets a surface's own text for a state.
type Describe = Box<dyn Fn(&ShortcutState, ShortcutPath)>;

/// How a surface draws the key and words its button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Onboarding's step: large key caps, a button that names the shortcut.
    Onboarding,
    /// A settings row: the key as dim text, a one-word button.
    Row,
}

pub struct ShortcutControl {
    keys: gtk::Box,
    button: gtk::Button,
    overlay: adw::ToastOverlay,
    surface: Surface,
    describe: Describe,
    proxy: RefCell<Option<gio::DBusProxy>>,
    desktop: Option<DesktopShortcut>,
    path: Cell<ShortcutPath>,
    state: RefCell<ShortcutState>,
    busy: Cell<bool>,
    default_pending: Cell<bool>,
    /// The control itself, for the bind a refresh starts.
    me: std::rc::Weak<Self>,
    changed: RefCell<Option<Box<dyn Fn()>>>,
}

impl ShortcutControl {
    /// Drive `keys` and `button` from the daemon's `Shortcut`. `describe` sets
    /// the surface's own text for each state. The button's handler owns the
    /// control, so it lives as long as the button.
    pub fn attach(
        keys: gtk::Box,
        button: gtk::Button,
        overlay: adw::ToastOverlay,
        surface: Surface,
        describe: Describe,
    ) -> Rc<Self> {
        let control = Rc::new_cyclic(|me| Self {
            keys,
            button: button.clone(),
            overlay,
            surface,
            describe,
            proxy: RefCell::new(None),
            desktop: DesktopShortcut::open(),
            path: Cell::new(ShortcutPath::Portal),
            state: RefCell::new(ShortcutState::NotRunning),
            busy: Cell::new(false),
            default_pending: Cell::new(false),
            me: me.clone(),
            changed: RefCell::default(),
        });
        control.render();
        if let Some(desktop) = &control.desktop {
            let weak = Rc::downgrade(&control);
            desktop.connect_changed(move || {
                if let Some(control) = weak.upgrade() {
                    control.refresh();
                }
            });
        }
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
        control
    }

    /// Run `changed` after every state the control shows.
    pub fn connect_changed(&self, changed: Box<dyn Fn()>) {
        self.changed.replace(Some(changed));
    }

    /// The daemon runs with no key bound: dictation cannot be triggered yet.
    pub fn needs_key(&self) -> bool {
        *self.state.borrow() == ShortcutState::Unbound
    }

    /// The unique name owning the daemon's name, if any. `None` while there
    /// is no session bus to watch it on.
    pub fn owner(&self) -> Option<Option<String>> {
        self.proxy
            .borrow()
            .as_ref()
            .map(|proxy| proxy.name_owner().map(|owner| owner.to_string()))
    }

    /// Set the default key once the daemon says how it is activated, unless
    /// a key is already bound or taken: under control install it, under the
    /// portal raise the portal's dialog offering it.
    pub fn install_default(&self) {
        self.default_pending.set(true);
        self.refresh();
    }

    fn refresh(&self) {
        let property = |name: &str| {
            self.proxy
                .borrow()
                .as_ref()
                .and_then(|proxy| proxy.cached_property(name))
                .and_then(|value| value.get::<String>())
        };
        let owned = self
            .proxy
            .borrow()
            .as_ref()
            .is_some_and(|proxy| proxy.name_owner().is_some());
        let activation = property("Activation");
        let path = ShortcutPath::from_activation(activation.as_deref());
        let state = match path {
            ShortcutPath::Portal => ShortcutState::observe(owned, property("Shortcut").as_deref()),
            ShortcutPath::Control => ShortcutState::observe_control(
                owned,
                self.desktop
                    .as_ref()
                    .and_then(DesktopShortcut::binding)
                    .as_deref(),
            ),
        };
        self.path.set(path);
        self.state.replace(state.clone());
        self.render();
        if self.default_pending.get() {
            let available = self
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.conflict(DEFAULT_ACCELERATOR).is_none());
            match default_key(activation.as_deref(), &state, available) {
                DefaultKey::Wait => {}
                DefaultKey::Install => {
                    self.default_pending.set(false);
                    self.install(DEFAULT_ACCELERATOR);
                }
                DefaultKey::Bind => {
                    self.default_pending.set(false);
                    if let Some(control) = self.me.upgrade() {
                        control.bind(false);
                    }
                }
                DefaultKey::Leave => self.default_pending.set(false),
            }
        }
    }

    fn render(&self) {
        let state = self.state.borrow().clone();
        let path = self.path.get();
        (self.describe)(&state, path);

        while let Some(child) = self.keys.first_child() {
            self.keys.remove(&child);
        }
        match &state {
            ShortcutState::Bound(description) => {
                self.keys.set_visible(true);
                fill_keys(&self.keys, description, self.surface);
            }
            _ => self.keys.set_visible(false),
        }

        let bound = matches!(state, ShortcutState::Bound(_) | ShortcutState::Unpublished);
        let label = match (self.surface, bound) {
            (Surface::Onboarding, true) => gettextrs::gettext("Change shortcut"),
            (Surface::Onboarding, false) => gettextrs::gettext("Set up shortcut"),
            (Surface::Row, true) => gettextrs::gettext("Change"),
            (Surface::Row, false) => gettextrs::gettext("Set up"),
        };
        let help = match (path, &state) {
            (ShortcutPath::Control, ShortcutState::Bound(_)) => {
                gettextrs::gettext("Press a different keyboard shortcut for dictation.")
            }
            (ShortcutPath::Control, ShortcutState::Unbound | ShortcutState::NotRunning) => {
                gettextrs::gettext("Add a keyboard shortcut for dictation to the desktop.")
            }
            (_, ShortcutState::Bound(_) | ShortcutState::Unpublished) => gettextrs::gettext(
                "Open Myna in the desktop's Apps settings, where the dictation shortcut is changed.",
            ),
            (_, ShortcutState::Unbound | ShortcutState::NotRunning) => gettextrs::gettext(
                "Open the desktop's dialog to confirm a keyboard shortcut for dictation.",
            ),
        };
        self.button.set_label(&label);
        self.button
            .update_property(&[gtk::accessible::Property::Description(&help)]);
        self.button
            .set_sensitive(!self.busy.get() && state != ShortcutState::NotRunning);
        // Onboarding cannot finish usefully without a key, so setting one up
        // is the step's main action until there is one.
        if self.surface == Surface::Onboarding {
            let main = state == ShortcutState::Unbound;
            set_class(&self.button, "suggested-action", main);
        }
        if let Some(changed) = &*self.changed.borrow() {
            changed();
        }
    }

    fn activate(self: &Rc<Self>) {
        let state = self.state.borrow().clone();
        match (self.path.get(), state) {
            (_, ShortcutState::NotRunning) => {}
            (ShortcutPath::Control, ShortcutState::Unbound) => self.claim(DEFAULT_ACCELERATOR),
            (ShortcutPath::Control, _) => self.change(),
            (ShortcutPath::Portal, ShortcutState::Unbound) => self.bind(true),
            (ShortcutPath::Portal, _) => {
                self.open_settings(&format!("applications {MYNA_SNAP}_{MYNA_SNAP}"))
            }
        }
    }

    /// Capture a new key for the desktop shortcut.
    fn change(self: &Rc<Self>) {
        let dialog = crate::ui::ShortcutDialog::new();
        let control = Rc::downgrade(self);
        dialog.connect_captured(move |accelerator| {
            let control = control.upgrade()?;
            if let Some(reason) = control.reserved(accelerator) {
                return Some(reason);
            }
            control.claim(accelerator);
            None
        });
        dialog.present(self.overlay.root().as_ref());
    }

    /// Why `accelerator` cannot be taken, when the desktop reserves it.
    fn reserved(&self, accelerator: &str) -> Option<String> {
        let conflict = self.desktop.as_ref()?.conflict(accelerator)?;
        conflict.reserved.then(|| {
            gettextrs::gettext("{keys} is reserved for “{action}”. Press a different shortcut.")
                .replace("{keys}", &key_label(accelerator))
                .replace("{action}", &conflict.action)
        })
    }

    /// Install `accelerator`, first asking to take it from whatever desktop
    /// shortcut holds it.
    fn claim(self: &Rc<Self>, accelerator: &str) {
        let Some(conflict) = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.conflict(accelerator))
        else {
            self.install(accelerator);
            return;
        };
        if let Some(reason) = self.reserved(accelerator) {
            self.overlay.add_toast(adw::Toast::new(&reason));
            return;
        }
        let body = gettextrs::gettext(
            "{keys} is already used for “{action}”. Replacing it removes it from there.",
        )
        .replace("{keys}", &key_label(accelerator))
        .replace("{action}", &conflict.action);
        let alert =
            adw::AlertDialog::new(Some(&gettextrs::gettext("Replace Shortcut?")), Some(&body));
        alert.add_response("cancel", &gettextrs::gettext("Cancel"));
        alert.add_response("replace", &gettextrs::gettext("Replace"));
        alert.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        alert.set_close_response("cancel");
        let control = Rc::downgrade(self);
        let accelerator = accelerator.to_owned();
        alert.connect_response(None, move |_, response| {
            let Some(control) = control.upgrade().filter(|_| response == "replace") else {
                return;
            };
            let released = control
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.release(&conflict).is_ok());
            if released {
                control.install(&accelerator);
            } else {
                control
                    .overlay
                    .add_toast(adw::Toast::new(&gettextrs::gettext(
                        "Could not set up the shortcut",
                    )));
            }
        });
        alert.present(self.overlay.root().as_ref());
    }

    /// Bind `accelerator` to the snap's toggle app, which pokes the daemon's
    /// control socket.
    fn install(&self, accelerator: &str) {
        let installed = self.desktop.as_ref().is_some_and(|desktop| {
            desktop
                .install(
                    &gettextrs::gettext("Dictation"),
                    &format!("/snap/bin/{MYNA_SNAP}.toggle"),
                    accelerator,
                )
                .is_ok()
        });
        if !installed {
            self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                "Could not set up the shortcut",
            )));
        }
        self.refresh();
    }

    /// Ask the daemon to bind. The portal keys a binding by the caller's app
    /// id, so only the daemon can make one it will see. Only a bind the user
    /// asked for reports a failure: one setup raised was answered in the
    /// portal's dialog, and the step's button stays to try again.
    fn bind(self: &Rc<Self>, asked: bool) {
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
            if let (Some(detail), false) = (&failure, asked) {
                glib::g_message!(crate::LOG_DOMAIN, "shortcut: setup's bind: {detail}");
            }
            if let Some(detail) = failure.filter(|_| asked) {
                let heading = gettextrs::gettext("Could not set up the shortcut");
                crate::ui::OperationErrorDialog::new(&heading, &heading, &detail)
                    .present(control.overlay.root().as_ref());
            }
            control.refresh();
        });
    }

    /// GNOME rebinds portal shortcuts on the app's page under Apps.
    fn open_settings(&self, panel: &str) {
        let command = format!("gnome-control-center {panel}");
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

pub(crate) fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// The onboarding step's sentence for `state`.
pub fn onboarding_description(state: &ShortcutState, path: ShortcutPath) -> String {
    match state {
        ShortcutState::Unbound if path == ShortcutPath::Control => {
            gettextrs::gettext("Set up a keyboard shortcut to trigger Dictation.")
        }
        ShortcutState::Bound(_) => {
            gettextrs::gettext("You can trigger Dictation anytime by using the keyboard shortcut:")
        }
        ShortcutState::Unbound => gettextrs::gettext(
            "Set up a keyboard shortcut to trigger Dictation. You will be asked to confirm it.",
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

/// The first accelerator in `description` drawn for `surface`, or the
/// description itself when it names none GTK can parse.
pub(crate) fn fill_keys(keys: &gtk::Box, description: &str, surface: Surface) {
    let caps = accelerators(description)
        .first()
        .and_then(|accelerator| key_caps(accelerator));
    let Some(caps) = caps else {
        let label = gtk::Label::new(Some(description));
        if surface == Surface::Row {
            label.add_css_class("dim-label");
        }
        keys.append(&label);
        return;
    };
    if surface == Surface::Row {
        let label = gtk::Label::new(Some(&caps.join(" + ")));
        label.add_css_class("dim-label");
        keys.append(&label);
        return;
    }
    for (index, cap) in caps.iter().enumerate() {
        if index > 0 {
            keys.append(&gtk::Label::new(Some("+")));
        }
        let label = gtk::Label::new(Some(cap));
        label.add_css_class("keycap");
        keys.append(&label);
    }
}

/// `accelerator` the way the key caps read, such as `Super+L`.
fn key_label(accelerator: &str) -> String {
    key_caps(accelerator).map_or_else(|| accelerator.to_owned(), |caps| caps.join("+"))
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
