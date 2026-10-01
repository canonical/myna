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
    accelerators, bind_end, default_key, BindEnd, BindReply, DefaultKey, DialogHint, ShortcutPath,
    ShortcutState, DEFAULT_ACCELERATOR,
};

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
/// The daemon waits for the portal's dialog as long as it stays up.
const BIND_TIMEOUT_MS: i32 = i32::MAX;
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Sets a surface's own text for a state and what it says about a portal
/// dialog.
type Describe = Box<dyn Fn(&ShortcutState, ShortcutPath, DialogHint)>;

/// What every surface in this process knows about its own binds: the daemon
/// publishes only its dialog, and an older daemon publishes nothing, so the
/// Myna page row and the wizard learn of each other's dialogs here.
#[derive(Default)]
struct Local {
    in_flight: Cell<usize>,
    /// A dialog one of them raised may be on screen with nobody waiting.
    left_open: Cell<bool>,
    controls: RefCell<Vec<std::rc::Weak<ShortcutControl>>>,
}

thread_local! {
    static LOCAL: Local = Local::default();
}

fn local_in_flight() -> bool {
    LOCAL.with(|local| local.in_flight.get() > 0)
}

fn local_left_open() -> bool {
    LOCAL.with(|local| local.left_open.get())
}

/// Apply `change` and redraw every live surface.
fn update_local(change: impl FnOnce(&Local)) {
    let controls = LOCAL.with(|local| {
        change(local);
        let mut controls = local.controls.borrow_mut();
        controls.retain(|control| control.strong_count() > 0);
        controls
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect::<Vec<_>>()
    });
    for control in controls {
        control.render();
    }
}

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
    /// Weak: the button's handler owns the control, and the overlay holds
    /// the button, so strong references would keep a closed window alive.
    button: glib::WeakRef<gtk::Button>,
    overlay: glib::WeakRef<adw::ToastOverlay>,
    surface: Surface,
    describe: Describe,
    proxy: RefCell<Option<gio::DBusProxy>>,
    desktop: Option<DesktopShortcut>,
    path: Cell<ShortcutPath>,
    state: RefCell<ShortcutState>,
    /// This surface's bind is waiting on its dialog.
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
            button: button.downgrade(),
            overlay: overlay.downgrade(),
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
        LOCAL.with(|local| local.controls.borrow_mut().push(Rc::downgrade(&control)));
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
        if matches!(state, ShortcutState::Bound(_)) && local_left_open() {
            LOCAL.with(|local| local.left_open.set(false));
        }
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
                    // Arriving raises no dialog beside one that may be up.
                    if self.binding() || local_left_open() {
                        return;
                    }
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
        let hint = if self.busy.get() {
            DialogHint::Own
        } else if self.dialog_open() || local_in_flight() {
            DialogHint::OpenElsewhere
        } else if local_left_open() {
            DialogHint::MaybeLeftOpen
        } else {
            DialogHint::None
        };
        (self.describe)(&state, path, hint);

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
        if let Some(button) = self.button.upgrade() {
            button.set_label(&label);
            button.update_property(&[gtk::accessible::Property::Description(&help)]);
            button.set_sensitive(!self.binding() && state != ShortcutState::NotRunning);
            // Onboarding cannot finish usefully without a key, so setting one
            // up is the step's main action until there is one.
            if self.surface == Surface::Onboarding {
                let main = state == ShortcutState::Unbound;
                set_class(&button, "suggested-action", main);
            }
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
        dialog.present(self.root().as_ref());
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
            self.toast(adw::Toast::new(&reason));
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
                control.toast(adw::Toast::new(&gettextrs::gettext(
                    "Could not set up the shortcut",
                )));
            }
        });
        alert.present(self.root().as_ref());
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
            self.toast(adw::Toast::new(&gettextrs::gettext(
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
        if self.binding() {
            return;
        }
        self.busy.set(true);
        // Asking again is the user's answer to a dialog that may be left.
        update_local(|local| {
            local.in_flight.set(local.in_flight.get() + 1);
            local.left_open.set(false);
        });
        let weak = Rc::downgrade(self);
        let window = self
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        export_parent(window.as_ref(), move |parent| {
            glib::spawn_future_local(async move {
                let (reply, legacy) = bind_call(&proxy, parent.id()).await;
                drop(parent);
                // A surface closed under its dialog still releases its hold.
                let control = weak.upgrade();
                if let Some(control) = &control {
                    control.busy.set(false);
                }
                let end = settle_bind(reply, legacy);
                if let Some(control) = control {
                    control.bound(end, asked);
                }
            });
        });
    }

    fn bound(&self, end: BindEnd, asked: bool) {
        if let (BindEnd::Failed(detail), true) = (end, asked) {
            self.report_failure(detail);
        }
        self.refresh();
    }

    /// A toast whose Details open the daemon's own words.
    fn report_failure(&self, detail: String) {
        let heading = gettextrs::gettext("Could not set up the shortcut");
        let summary =
            gettextrs::gettext("The desktop did not set up a keyboard shortcut for Dictation.");
        let toast = adw::Toast::builder()
            .title(crate::markup::escape_markup(&heading))
            .button_label(gettextrs::gettext("Details"))
            .build();
        toast.connect_button_clicked({
            let overlay = self.overlay.clone();
            move |_| {
                if let Some(overlay) = overlay.upgrade() {
                    crate::ui::OperationErrorDialog::new(&heading, &summary, &detail)
                        .present(Some(overlay.upcast_ref::<gtk::Widget>()));
                }
            }
        });
        self.toast(toast);
    }

    fn toast(&self, toast: adw::Toast) {
        if let Some(overlay) = self.overlay.upgrade() {
            overlay.add_toast(toast);
        }
    }

    fn root(&self) -> Option<gtk::Root> {
        self.overlay.upgrade().and_then(|overlay| overlay.root())
    }

    /// A portal dialog is up, from this process or the daemon's: the
    /// control waits for its answer.
    pub fn binding(&self) -> bool {
        self.busy.get() || local_in_flight() || self.dialog_open()
    }

    /// The daemon's `ShortcutDialog`: a bind's dialog is up, from any client.
    fn dialog_open(&self) -> bool {
        self.proxy
            .borrow()
            .as_ref()
            .filter(|proxy| proxy.name_owner().is_some())
            .and_then(|proxy| proxy.cached_property("ShortcutDialog"))
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false)
    }

    /// GNOME rebinds portal shortcuts on the app's page under Apps.
    fn open_settings(&self, panel: &str) {
        let command = format!("gnome-control-center {panel}");
        let launched =
            gio::AppInfo::create_from_commandline(&command, None, gio::AppInfoCreateFlags::NONE)
                .and_then(|app| app.launch(&[], gio::AppLaunchContext::NONE));
        if launched.is_err() {
            self.toast(adw::Toast::new(&gettextrs::gettext(
                "Could not open the desktop settings",
            )));
        }
    }
}

/// Judge a finished bind and release this process's hold on the dialog.
fn settle_bind(reply: Result<glib::Variant, glib::Error>, legacy: bool) -> BindEnd {
    let reply = match reply {
        Ok(reply) => match reply.get::<(bool, String)>() {
            Some((ok, message)) => BindReply::Answered { ok, message },
            None => BindReply::Failed(format!("unexpected reply {reply}")),
        },
        Err(error) if error.matches(gio::DBusError::NoReply) => BindReply::DaemonGone,
        Err(error) => BindReply::Failed(error.message().to_owned()),
    };
    if !matches!(reply, BindReply::Answered { ok: true, .. }) {
        glib::g_message!(crate::LOG_DOMAIN, "shortcut: bind: {reply:?}");
    }
    let end = bind_end(reply, legacy);
    update_local(|local| {
        local.in_flight.set(local.in_flight.get().saturating_sub(1));
        if end == BindEnd::LeftOpen {
            local.left_open.set(true);
        }
    });
    end
}

/// `BindShortcutWithParent`, or `BindShortcut` on a daemon that predates it,
/// with whether the older call was made.
async fn bind_call(
    proxy: &gio::DBusProxy,
    parent: &str,
) -> (Result<glib::Variant, glib::Error>, bool) {
    // Empty asks the daemon for its default trigger.
    let reply = proxy
        .call_future(
            "BindShortcutWithParent",
            Some(&("", parent).to_variant()),
            gio::DBusCallFlags::NONE,
            BIND_TIMEOUT_MS,
        )
        .await;
    match reply {
        Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
            let reply = proxy
                .call_future(
                    "BindShortcut",
                    Some(&("",).to_variant()),
                    gio::DBusCallFlags::NONE,
                    BIND_TIMEOUT_MS,
                )
                .await;
            (reply, true)
        }
        reply => (reply, false),
    }
}

/// `window` as a portal parent-window identifier, handed to `then`. On
/// Wayland the xdg-foreign handle stays exported until the [`Parent`] drops;
/// with no window, or none the portal could find, the id is empty.
fn export_parent(window: Option<&gtk::Window>, then: impl FnOnce(Parent) + 'static) {
    let surface = window.and_then(|window| window.surface());
    if let Some(toplevel) = surface
        .as_ref()
        .and_then(|surface| surface.downcast_ref::<gdk4_wayland::WaylandToplevel>())
    {
        let then = Rc::new(RefCell::new(Some(then)));
        let requested = toplevel.export_handle({
            let then = then.clone();
            move |toplevel, handle| {
                let Some(then) = then.take() else {
                    if let Ok(handle) = handle {
                        toplevel.drop_exported_handle(handle);
                    }
                    return;
                };
                then(match handle {
                    Ok(handle) => Parent {
                        id: format!("wayland:{handle}"),
                        exported: Some((toplevel.clone(), handle.to_owned())),
                    },
                    Err(error) => {
                        glib::g_message!(crate::LOG_DOMAIN, "shortcut: no parent: {error}");
                        Parent::none()
                    }
                });
            }
        });
        if !requested {
            if let Some(then) = then.take() {
                then(Parent::none());
            }
            return;
        }
        // A compositor that never answers must not hold the button forever.
        glib::timeout_add_local_once(EXPORT_TIMEOUT, move || {
            if let Some(then) = then.take() {
                glib::g_message!(crate::LOG_DOMAIN, "shortcut: no parent: export timed out");
                then(Parent::none());
            }
        });
        return;
    }
    if let Some(surface) = surface
        .as_ref()
        .and_then(|surface| surface.downcast_ref::<gdk4_x11::X11Surface>())
    {
        then(Parent {
            id: format!("x11:{:x}", surface.xid()),
            exported: None,
        });
        return;
    }
    then(Parent::none());
}

/// A portal parent-window identifier, unexported on drop.
struct Parent {
    id: String,
    exported: Option<(gdk4_wayland::WaylandToplevel, String)>,
}

impl Parent {
    fn none() -> Self {
        Self {
            id: String::new(),
            exported: None,
        }
    }

    fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        // A destroyed surface took its exports with it.
        if let Some((toplevel, handle)) = self.exported.take() {
            if !toplevel.is_destroyed() {
                toplevel.drop_exported_handle(&handle);
            }
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
pub fn onboarding_description(
    state: &ShortcutState,
    path: ShortcutPath,
    hint: DialogHint,
) -> String {
    match state {
        ShortcutState::Unbound if hint == DialogHint::OpenElsewhere => gettextrs::gettext(
            "The desktop's dialog to confirm a keyboard shortcut is already open. Answer it to continue.",
        ),
        ShortcutState::Unbound if hint == DialogHint::MaybeLeftOpen => gettextrs::gettext(
            "If the desktop's dialog to confirm a keyboard shortcut is no longer open, set up the shortcut again.",
        ),
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
pub fn row_subtitle(state: &ShortcutState, hint: DialogHint) -> String {
    match state {
        ShortcutState::Unbound if matches!(hint, DialogHint::Own | DialogHint::OpenElsewhere) => {
            gettextrs::gettext("Waiting for the desktop's shortcut dialog")
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_waits_on_its_own_dialog_as_on_any_other() {
        let waiting = row_subtitle(&ShortcutState::Unbound, DialogHint::OpenElsewhere);
        assert_eq!(
            row_subtitle(&ShortcutState::Unbound, DialogHint::Own),
            waiting
        );
        assert_ne!(
            row_subtitle(&ShortcutState::Unbound, DialogHint::None),
            waiting
        );
    }
}
