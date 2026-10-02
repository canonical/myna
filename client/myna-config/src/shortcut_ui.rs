//! GTK binding for the dictation shortcut, shared by the onboarding step and
//! the Myna page.
//!
//! State comes from a live proxy on `com.canonical.Myna.Dictation`, so a daemon
//! starting, a key bound, or a rebind in the desktop's settings shows up
//! without a refresh. Under control activation the key is the desktop custom
//! shortcut, watched the same way.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib::translate::IntoGlib;
use gtk::{gdk, glib};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::adapters::desktop_shortcut::DesktopShortcut;
use crate::adapters::portal_shortcuts::{self, PortalShortcuts, Store, Taken, SHORTCUT_ID};
use crate::onboarding::MYNA_SNAP;
use crate::shortcut::{
    bind_end, button_action, default_key, portal_trigger, trigger_key, BindEnd, BindReply,
    ButtonAction, DefaultKey, DialogHint, ShortcutPath, ShortcutState, DEFAULT_ACCELERATOR,
};

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
/// The daemon waits for the portal's dialog as long as it stays up.
const BIND_TIMEOUT_MS: i32 = i32::MAX;
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long the dialog's new key may take to reach this process from dconf.
const STORED_KEY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Sets a surface's own text for a state and what it says about a portal
/// dialog.
type Describe = Box<dyn Fn(&ShortcutState, ShortcutPath, DialogHint)>;
/// Told whether a shortcut dialog is up.
type DialogWatch = Box<dyn Fn(bool)>;

/// What every surface in this process knows about its own binds: the daemon
/// publishes only its dialog, and an older daemon publishes nothing, so the
/// Myna page row and the wizard learn of each other's dialogs here.
#[derive(Default)]
struct Local {
    in_flight: Cell<usize>,
    /// A dialog one of them raised may be on screen with nobody waiting.
    left_open: Cell<bool>,
    controls: RefCell<Vec<std::rc::Weak<ShortcutControl>>>,
    /// Told whether a shortcut dialog is up whenever that changes.
    dialog_watchers: RefCell<Vec<DialogWatch>>,
    dialog_told: Cell<Option<bool>>,
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

/// A portal dialog is up, as any live surface sees it: one of this
/// process's binds, or the daemon's `ShortcutDialog`.
pub fn dialog_up() -> bool {
    let controls = LOCAL.with(|local| {
        local
            .controls
            .borrow()
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect::<Vec<_>>()
    });
    local_in_flight() || controls.iter().any(|control| control.binding())
}

/// Run `watch` with [`dialog_up`] now and whenever it changes, for entries
/// outside a surface that open a shortcut dialog too, such as `win.setup`.
pub fn watch_dialog(watch: impl Fn(bool) + 'static) {
    watch(dialog_up());
    LOCAL.with(|local| local.dialog_watchers.borrow_mut().push(Box::new(watch)));
}

fn tell_dialog_watchers() {
    let up = dialog_up();
    LOCAL.with(|local| {
        if local.dialog_told.replace(Some(up)) != Some(up) {
            for watch in local.dialog_watchers.borrow().iter() {
                watch(up);
            }
        }
    });
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
    /// Onboarding's step: large key caps, a button that names the shortcut,
    /// and room to capture a new key in place.
    Onboarding,
    /// A settings row: the key as dim text, a one-word button.
    Row,
}

/// A surface's widgets for capturing a key in place of its key caps. `stack`
/// shows "idle" (the caps and the button) or "capture"; `room` shows a
/// two-line placeholder or `refusal`.
pub struct InPlace {
    pub stack: gtk::Stack,
    pub field: gtk::Label,
    pub room: gtk::Stack,
    pub refusal: gtk::Label,
    pub cancel: gtk::Button,
}

/// Weak, as the button: the stack holds the button whose handler owns the
/// control.
struct InPlaceRefs {
    stack: glib::WeakRef<gtk::Stack>,
    field: glib::WeakRef<gtk::Label>,
    room: glib::WeakRef<gtk::Stack>,
    refusal: glib::WeakRef<gtk::Label>,
    cancel: glib::WeakRef<gtk::Button>,
}

pub struct ShortcutControl {
    keys: gtk::Box,
    /// Weak: the button's handler owns the control, and the overlay holds
    /// the button, so strong references would keep a closed window alive.
    button: glib::WeakRef<gtk::Button>,
    overlay: glib::WeakRef<adw::ToastOverlay>,
    in_place: Option<InPlaceRefs>,
    surface: Surface,
    describe: Describe,
    proxy: RefCell<Option<gio::DBusProxy>>,
    desktop: Option<DesktopShortcut>,
    path: Cell<ShortcutPath>,
    state: RefCell<ShortcutState>,
    /// This surface's bind is waiting on its dialog.
    busy: Cell<bool>,
    /// The key controller and window of a capture in place.
    capture: RefCell<Option<(gtk::Window, gtk::EventControllerKey)>>,
    /// Why the last key pressed during a capture was refused.
    refusal: RefCell<Option<String>>,
    /// The capture's key is waiting on the replace question.
    asking: Cell<bool>,
    default_pending: Cell<bool>,
    /// The control itself, for the bind a refresh starts.
    me: std::rc::Weak<Self>,
    changed: RefCell<Option<Box<dyn Fn()>>>,
}

impl ShortcutControl {
    /// Drive `keys` and `button` from the daemon's `Shortcut`. `describe` sets
    /// the surface's own text for each state; with `in_place` a desktop
    /// shortcut is captured there rather than in a dialog. The button's
    /// handler owns the control, so it lives as long as the button.
    pub fn attach(
        keys: gtk::Box,
        button: gtk::Button,
        overlay: adw::ToastOverlay,
        surface: Surface,
        in_place: Option<InPlace>,
        describe: Describe,
    ) -> Rc<Self> {
        let cancel = in_place.as_ref().map(|in_place| in_place.cancel.clone());
        let control = Rc::new_cyclic(|me| Self {
            keys,
            button: button.downgrade(),
            overlay: overlay.downgrade(),
            in_place: in_place.map(|in_place| InPlaceRefs {
                stack: in_place.stack.downgrade(),
                field: in_place.field.downgrade(),
                room: in_place.room.downgrade(),
                refusal: in_place.refusal.downgrade(),
                cancel: in_place.cancel.downgrade(),
            }),
            surface,
            describe,
            proxy: RefCell::new(None),
            desktop: DesktopShortcut::open(),
            path: Cell::new(ShortcutPath::Portal),
            state: RefCell::new(ShortcutState::NotRunning),
            busy: Cell::new(false),
            capture: RefCell::default(),
            refusal: RefCell::default(),
            asking: Cell::new(false),
            default_pending: Cell::new(false),
            me: me.clone(),
            changed: RefCell::default(),
        });
        LOCAL.with(|local| local.controls.borrow_mut().push(Rc::downgrade(&control)));
        control.render();
        // Leaving the page, or closing its window, ends a capture.
        if let Some(cancel) = cancel {
            cancel.connect_unmap({
                let weak = Rc::downgrade(&control);
                move |_| {
                    if let Some(control) = weak.upgrade() {
                        control.end_capture(false);
                    }
                }
            });
            let weak = Rc::downgrade(&control);
            cancel.connect_clicked(move |_| {
                if let Some(control) = weak.upgrade() {
                    control.end_capture(true);
                }
            });
        }
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

    /// A dialog or a capture in place waits on the user: the step cannot
    /// finish under it.
    pub fn held(&self) -> bool {
        self.binding() || self.capturing()
    }

    pub fn capturing(&self) -> bool {
        self.capture.borrow().is_some()
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
            let dialog_up = self.binding() || local_left_open();
            match default_key(activation.as_deref(), &state, available, dialog_up) {
                DefaultKey::Wait => {}
                DefaultKey::Install => {
                    self.default_pending.set(false);
                    self.install(DEFAULT_ACCELERATOR);
                }
                DefaultKey::Bind => {
                    self.default_pending.set(false);
                    if let Some(control) = self.me.upgrade() {
                        control.bind(false, false);
                    }
                }
                DefaultKey::Leave => self.default_pending.set(false),
            }
        }
    }

    fn render(&self) {
        let state = self.state.borrow().clone();
        let path = self.path.get();
        let capturing = self.capturing();
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
        if let Some(in_place) = &self.in_place {
            self.render_capture(
                in_place,
                capturing,
                matches!(state, ShortcutState::Bound(_)),
            );
        }
        match &state {
            ShortcutState::Bound(description) => {
                self.keys.set_visible(true);
                let stored = match path {
                    ShortcutPath::Control => Some(description.clone()),
                    ShortcutPath::Portal => stored_key(),
                };
                fill_keys(&self.keys, description, stored.as_deref(), self.surface);
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
            (ShortcutPath::Control, _) if self.in_place.is_some() => {
                gettextrs::gettext("Press a keyboard shortcut for dictation.")
            }
            (ShortcutPath::Control, ShortcutState::Bound(_)) => {
                gettextrs::gettext("Press a different keyboard shortcut for dictation.")
            }
            (ShortcutPath::Control, ShortcutState::Unbound | ShortcutState::NotRunning) => {
                gettextrs::gettext("Add a keyboard shortcut for dictation to the desktop.")
            }
            (_, ShortcutState::Bound(_) | ShortcutState::Unpublished) => gettextrs::gettext(
                "Open the desktop's dialog to change the keyboard shortcut for dictation.",
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
        tell_dialog_watchers();
    }

    /// Show the capture or the idle key and button, and the field's words.
    fn render_capture(&self, in_place: &InPlaceRefs, capturing: bool, changing: bool) {
        let refusal = self.refusal.borrow();
        if let Some(stack) = in_place.stack.upgrade() {
            stack.set_visible_child_name(if capturing { "capture" } else { "idle" });
        }
        if let Some(field) = in_place.field.upgrade() {
            field.set_label(&if changing {
                gettextrs::gettext("Press the new shortcut…")
            } else {
                gettextrs::gettext("Press a shortcut…")
            });
            set_class(&field, "refused", refusal.is_some());
        }
        if let (Some(room), Some(label)) = (in_place.room.upgrade(), in_place.refusal.upgrade()) {
            match refusal.as_deref() {
                Some(reason) => {
                    label.set_label(reason);
                    room.set_visible_child(&label);
                }
                None => {
                    if let Some(placeholder) = room.first_child() {
                        room.set_visible_child(&placeholder);
                    }
                }
            }
        }
        if let Some(cancel) = in_place.cancel.upgrade() {
            cancel.update_property(
                &[gtk::accessible::Property::Description(&gettextrs::gettext(
                    "Keep the shortcut as it was.",
                ))],
            );
        }
    }

    fn activate(self: &Rc<Self>) {
        let action = button_action(
            self.path.get(),
            &self.state.borrow(),
            self.in_place.is_some(),
            self.capturing(),
        );
        match action {
            ButtonAction::Nothing => {}
            ButtonAction::ClaimDefault => self.claim(DEFAULT_ACCELERATOR, None),
            ButtonAction::Capture => self.start_capture(),
            ButtonAction::CancelCapture => self.end_capture(true),
            ButtonAction::CaptureDialog => self.change(),
            ButtonAction::Bind => self.bind(true, false),
            ButtonAction::Rebind => self.bind(true, true),
        }
    }

    /// Wait in place for the next key pressed anywhere in the window.
    fn start_capture(self: &Rc<Self>) {
        let Some(window) = self
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        else {
            return;
        };
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            weak.upgrade()
                .map_or(glib::Propagation::Proceed, |control| {
                    control.press(key, state)
                })
        });
        window.add_controller(keys.clone());
        // The desktop grabs the keys it uses (Super+L, the Calculator key)
        // before any window sees them, as in the dialog.
        if let Some(toplevel) = toplevel(&window) {
            toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
        }
        self.capture.replace(Some((window, keys)));
        self.refusal.replace(None);
        self.asking.set(false);
        self.render();
        self.focus_cancel();
    }

    /// Key events reach the window only from inside it, and Cancel is what
    /// Enter or Space should press.
    fn focus_cancel(&self) {
        if let Some(cancel) = self
            .in_place
            .as_ref()
            .and_then(|refs| refs.cancel.upgrade())
        {
            cancel.grab_focus();
        }
    }

    /// Stop capturing. `refocus` hands focus back to the button that started
    /// it, which the capture's Cancel had.
    fn end_capture(&self, refocus: bool) {
        let Some((window, keys)) = self.capture.take() else {
            return;
        };
        window.remove_controller(&keys);
        if let Some(toplevel) = toplevel(&window) {
            toplevel.restore_system_shortcuts();
        }
        self.refusal.replace(None);
        self.asking.set(false);
        self.render();
        if let Some(button) = self.button.upgrade().filter(|_| refocus) {
            button.grab_focus();
        }
    }

    /// A key pressed during a capture in place: take it, refuse it inline,
    /// or end the capture on Escape.
    pub fn press(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if !self.capturing() || self.asking.get() {
            return glib::Propagation::Proceed;
        }
        let accelerator = match capture_key(key, state) {
            Capture::Ignore => return glib::Propagation::Proceed,
            Capture::Cancel => {
                self.end_capture(true);
                return glib::Propagation::Stop;
            }
            Capture::Take(accelerator) => accelerator,
        };
        if let Some(reason) = self.reserved(&accelerator) {
            self.refusal.replace(Some(reason));
            self.render();
            return glib::Propagation::Stop;
        }
        if self.refusal.take().is_some() {
            self.render();
        }
        let weak = Rc::downgrade(self);
        self.asking.set(true);
        self.claim(
            &accelerator,
            Some(Box::new(move |taken| {
                let Some(control) = weak.upgrade() else {
                    return;
                };
                control.asking.set(false);
                if taken {
                    control.end_capture(true);
                } else {
                    // Declining the swap keeps waiting for a different key.
                    control.focus_cancel();
                }
            })),
        );
        glib::Propagation::Stop
    }

    /// Capture a new key for the desktop shortcut in a dialog.
    fn change(self: &Rc<Self>) {
        let dialog = crate::ui::ShortcutDialog::new();
        let control = Rc::downgrade(self);
        dialog.connect_captured(move |accelerator| {
            let control = control.upgrade()?;
            if let Some(reason) = control.reserved(accelerator) {
                return Some(reason);
            }
            control.claim(accelerator, None);
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
    /// shortcut holds it. `then` learns whether it was installed.
    fn claim(self: &Rc<Self>, accelerator: &str, then: Option<Box<dyn Fn(bool)>>) {
        let then = move |taken| {
            if let Some(then) = &then {
                then(taken);
            }
        };
        let Some(conflict) = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.conflict(accelerator))
        else {
            then(self.install(accelerator));
            return;
        };
        if let Some(reason) = self.reserved(accelerator) {
            self.toast(adw::Toast::new(&reason));
            then(false);
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
        // `then` waits for both the answer and the alert giving focus back,
        // which libadwaita versions signal in either order.
        let answer = Rc::new(Cell::new(None::<bool>));
        let closed = Rc::new(Cell::new(false));
        let then = Rc::new(then);
        alert.connect_response(None, {
            let (answer, closed, then) = (answer.clone(), closed.clone(), then.clone());
            move |_, response| {
                let taken = control
                    .upgrade()
                    .filter(|_| response == "replace")
                    .is_some_and(|control| control.replace(&conflict, &accelerator));
                answer.set(Some(taken));
                if closed.get() {
                    then(taken);
                }
            }
        });
        alert.connect_closed(move |_| {
            closed.set(true);
            if let Some(taken) = answer.get() {
                then(taken);
            }
        });
        alert.present(self.root().as_ref());
    }

    /// Take `accelerator` from the shortcut `conflict` names and install it.
    fn replace(
        &self,
        conflict: &crate::adapters::desktop_shortcut::Conflict,
        accelerator: &str,
    ) -> bool {
        let released = self
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.release(conflict).is_ok());
        if !released {
            self.toast(adw::Toast::new(&gettextrs::gettext(
                "Could not set up the shortcut",
            )));
            return false;
        }
        self.install(accelerator)
    }

    /// Bind `accelerator` to the snap's toggle app, which pokes the daemon's
    /// control socket. Whether it was installed.
    fn install(&self, accelerator: &str) -> bool {
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
        installed
    }

    /// Ask the daemon to bind. The portal keys a binding by the caller's app
    /// id, so only the daemon can make one it will see. Only a bind the user
    /// asked for reports a failure: one setup raised was answered in the
    /// portal's dialog, and the step's button stays to try again.
    ///
    /// `change` raises the dialog for a bound key: GNOME offers one only for
    /// a shortcut it stores no key for, so the stored entry is taken out
    /// while the dialog is up and put back unless the dialog stored a new
    /// one. Where GNOME keeps no such store, or nothing serves it, GNOME
    /// Settings changes it.
    fn bind(self: &Rc<Self>, asked: bool, change: bool) {
        if self.proxy.borrow().is_none() || self.binding() {
            return;
        }
        if !change {
            self.ask_bind(asked, None);
            return;
        }
        // Held while the store is checked.
        self.busy.set(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let change = Change::begin().await;
            let Some(control) = weak.upgrade() else {
                return;
            };
            control.busy.set(false);
            match change {
                Some(change) => control.ask_bind(asked, Some(change)),
                None => {
                    control.open_settings(&format!("applications {MYNA_SNAP}_{MYNA_SNAP}"));
                    control.refresh();
                }
            }
        });
    }

    fn ask_bind(self: &Rc<Self>, asked: bool, change: Option<Change>) {
        let Some(proxy) = self.proxy.borrow().clone() else {
            return;
        };
        let changing = change.is_some();
        let preferred = change
            .as_ref()
            .and_then(Change::current)
            .and_then(portal_trigger)
            .unwrap_or_default();
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
                let (reply, legacy) = bind_call(&proxy, &preferred, parent.id()).await;
                drop(parent);
                // A surface closed under its dialog still releases its hold.
                let control = weak.upgrade();
                if let Some(control) = &control {
                    control.busy.set(false);
                }
                let end = settle_bind(reply, legacy);
                let done = end == BindEnd::Done;
                if let Some(control) = control {
                    control.bound(end, asked, changing);
                }
                if let Some(change) = change {
                    change.finish(done).await;
                }
            });
        });
    }

    fn bound(&self, end: BindEnd, asked: bool, changing: bool) {
        if let (BindEnd::Failed(detail), true) = (end, asked) {
            self.report_failure(detail, changing);
        }
        self.refresh();
    }

    /// A toast whose Details open the daemon's own words.
    fn report_failure(&self, detail: String, changing: bool) {
        let (heading, summary) = bind_failure_words(changing);
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

    /// GNOME Settings changes portal shortcuts on the app's page under Apps.
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
        Ok(reply) => {
            if let Some((reason, message)) = reply.get::<(String, String)>() {
                BindReply::Reasoned { reason, message }
            } else if let Some((ok, message)) = reply.get::<(bool, String)>() {
                BindReply::Answered { ok, message }
            } else {
                BindReply::Failed(format!("unexpected reply {reply}"))
            }
        }
        Err(error) if error.matches(gio::DBusError::NoReply) => BindReply::DaemonGone,
        Err(error) => BindReply::Failed(error.message().to_owned()),
    };
    if !matches!(reply, BindReply::Answered { ok: true, .. })
        && !matches!(&reply, BindReply::Reasoned { reason, .. } if reason == "bound")
    {
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

/// Myna's keys taken out of GNOME's store while its dialog is up. The app
/// stays alive until they are settled, so closing the window under the
/// dialog still puts them back.
struct Change {
    store: Store,
    taken: Vec<(PortalShortcuts, Taken)>,
    _hold: Option<gio::ApplicationHoldGuard>,
}

impl Change {
    /// Take Myna's key out under every app id it may be filed under, so
    /// GNOME raises its dialog whichever one the daemon's session has. With
    /// nothing stored GNOME raises it anyway. `None` where GNOME keeps no
    /// store, or nothing serves it.
    async fn begin() -> Option<Self> {
        let store = Store::open()?;
        let bus = gio::bus_get_future(gio::BusType::Session).await.ok()?;
        if !portal_shortcuts::provider_present(&bus).await {
            return None;
        }
        let taken = store
            .myna_apps()
            .iter()
            .map(|app_id| store.app(app_id))
            .filter_map(|app| app.take(SHORTCUT_ID).map(|taken| (app, taken)))
            .collect();
        Some(Self {
            store,
            taken,
            _hold: gio::Application::default().map(|app| app.hold()),
        })
    }

    fn put_back(&mut self) {
        for (app, taken) in self.taken.drain(..) {
            app.put_back(taken);
        }
    }

    /// The key to offer: the one taken under `myna_myna`, else any.
    fn current(&self) -> Option<&str> {
        self.taken
            .first()
            .map(|(_, taken)| taken.accelerator.as_str())
    }

    /// The app ids holding a key now, which only the dialog can have stored.
    fn stored(&self) -> Vec<String> {
        self.store
            .myna_apps()
            .into_iter()
            .filter(|app_id| self.store.app(app_id).accelerator(SHORTCUT_ID).is_some())
            .collect()
    }

    /// After a bind: move the daemon's live grab to the key the dialog
    /// stored, and put back every key it did not replace.
    async fn finish(mut self, done: bool) {
        let mut stored = Vec::new();
        if done {
            let deadline = std::time::Instant::now() + STORED_KEY_TIMEOUT;
            loop {
                stored = self.stored();
                if !stored.is_empty() || std::time::Instant::now() >= deadline {
                    break;
                }
                glib::timeout_future(std::time::Duration::from_millis(100)).await;
            }
        }
        self.put_back();
        if stored.is_empty() {
            return;
        }
        // The dialog's session bound the key and closed; the session the
        // daemon listens on still holds the old one.
        let bus = match gio::bus_get_future(gio::BusType::Session).await {
            Ok(bus) => bus,
            Err(error) => {
                glib::g_message!(crate::LOG_DOMAIN, "shortcut: rebind: {error}");
                return;
            }
        };
        for app_id in stored {
            let shortcuts = self.store.app(&app_id).shortcuts();
            if let Err(error) = portal_shortcuts::rebind(&bus, &app_id, shortcuts).await {
                glib::g_message!(crate::LOG_DOMAIN, "shortcut: rebind {app_id}: {error}");
            }
        }
    }
}

impl Drop for Change {
    fn drop(&mut self) {
        self.put_back();
    }
}

/// `BindShortcutWithOutcome`, else on a daemon that predates it
/// `BindShortcutWithParent`, else `BindShortcut`, with whether that oldest
/// call was made. An empty `preferred` asks the daemon for its default
/// trigger.
async fn bind_call(
    proxy: &gio::DBusProxy,
    preferred: &str,
    parent: &str,
) -> (Result<glib::Variant, glib::Error>, bool) {
    let reply = proxy
        .call_future(
            "BindShortcutWithOutcome",
            Some(&(preferred, parent).to_variant()),
            gio::DBusCallFlags::NONE,
            BIND_TIMEOUT_MS,
        )
        .await;
    match reply {
        Err(error) if error.matches(gio::DBusError::UnknownMethod) => {}
        reply => return (reply, false),
    }
    let reply = proxy
        .call_future(
            "BindShortcutWithParent",
            Some(&(preferred, parent).to_variant()),
            gio::DBusCallFlags::NONE,
            BIND_TIMEOUT_MS,
        )
        .await;
    match reply {
        Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
            let reply = proxy
                .call_future(
                    "BindShortcut",
                    Some(&(preferred,).to_variant()),
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

fn toplevel(window: &gtk::Window) -> Option<gdk::Toplevel> {
    window.surface()?.downcast::<gdk::Toplevel>().ok()
}

/// What a key pressed while capturing a shortcut means.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Capture {
    /// Escape: keep the shortcut there was.
    Cancel,
    /// Typing, or a key no shortcut can be: let it through.
    Ignore,
    /// The accelerator to bind, as the desktop spells it.
    Take(String),
}

/// Judge a key pressed while capturing. A combination with Ctrl, Alt or Super
/// is taken, and so is a lone function or media key. Anything else, such as
/// a bare letter that would take over typing, is ignored.
pub(crate) fn capture_key(key: gdk::Key, state: gdk::ModifierType) -> Capture {
    let modifiers = state & gtk::accelerator_get_default_mod_mask();
    if key == gdk::Key::Escape && modifiers.is_empty() {
        return Capture::Cancel;
    }
    let key = key.to_lower();
    let chord = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK;
    if !(modifiers.intersects(chord) || stands_alone(key))
        || !gtk::accelerator_valid(key, modifiers)
    {
        return Capture::Ignore;
    }
    Capture::Take(accelerator_name(key, modifiers))
}

/// The XF86 media keys: Calculator, Mail, ...
const MEDIA_KEYS: std::ops::RangeInclusive<u32> = 0x1008_ff00..=0x1008_ffff;

/// Keys that type nothing and move nothing: F1 to F35 and the media keys.
fn stands_alone(key: gdk::Key) -> bool {
    (gdk::Key::F1.into_glib()..=gdk::Key::F35.into_glib()).contains(&key.into_glib())
        || MEDIA_KEYS.contains(&key.into_glib())
}

/// GTK names a media key without its `XF86` prefix, which the desktop's
/// keysym lookup does not know, so the binding would never fire.
fn accelerator_name(key: gdk::Key, modifiers: gdk::ModifierType) -> String {
    let name = gtk::accelerator_name(key, modifiers).to_string();
    match key.name() {
        Some(bare) if MEDIA_KEYS.contains(&key.into_glib()) && !bare.starts_with("XF86") => name
            .strip_suffix(bare.as_str())
            .map_or(name.clone(), |modifiers| format!("{modifiers}XF86{bare}")),
        _ => name,
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
            "A shortcut dialog is already open. Answer it to continue.",
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

/// The failure toast's heading and its Details' summary, for a bind that
/// set up a key or `changing` one.
fn bind_failure_words(changing: bool) -> (String, String) {
    if changing {
        (
            gettextrs::gettext("Could not change the shortcut"),
            gettextrs::gettext("The desktop did not change the keyboard shortcut for Dictation."),
        )
    } else {
        (
            gettextrs::gettext("Could not set up the shortcut"),
            gettextrs::gettext("The desktop did not set up a keyboard shortcut for Dictation."),
        )
    }
}

/// The key GNOME stores for the portal's binding, under whichever app id
/// holds one. `None` off GNOME, or while a change has taken it out.
fn stored_key() -> Option<String> {
    let store = Store::open()?;
    store
        .myna_apps()
        .iter()
        .find_map(|app_id| store.app(app_id).accelerator(SHORTCUT_ID))
}

/// Whether GTK knows `token` as a key that types no character, such as F2,
/// Print or XF86AudioPlay.
fn names_key(token: &str) -> bool {
    gtk::gdk::Key::from_name(token).is_some_and(|key| key.to_unicode().is_none_or(char::is_control))
}

/// The key of a binding described as `description` drawn for `surface`, or
/// the description itself when it names none. `stored` is the accelerator
/// the desktop keeps for it, as [`trigger_key`] weighs it.
pub(crate) fn fill_keys(
    keys: &gtk::Box,
    description: &str,
    stored: Option<&str>,
    surface: Surface,
) {
    let caps = trigger_key(description, stored, names_key).and_then(key_caps);
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

/// `accelerator` the way the settings row reads it, such as `Super + L`.
fn key_label(accelerator: &str) -> String {
    key_caps(accelerator).map_or_else(|| accelerator.to_owned(), |caps| caps.join(" + "))
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
    fn only_keys_that_type_nothing_are_named() {
        for key in ["F2", "F35", "Print", "Pause", "XF86AudioPlay", "Return"] {
            assert!(names_key(key), "{key}");
        }
        for word in ["Press", "j", "J", "space", "Appuyez", "sur", ""] {
            assert!(!names_key(word), "{word}");
        }
    }

    #[test]
    fn a_failed_change_says_change() {
        let (heading, summary) = bind_failure_words(true);
        assert_eq!(heading, "Could not change the shortcut");
        assert!(summary.contains("change"), "{summary}");
        assert_ne!(bind_failure_words(false), (heading, summary));
    }

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
