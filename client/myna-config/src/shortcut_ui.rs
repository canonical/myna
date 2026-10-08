//! GTK binding for the dictation shortcut, shared by the onboarding step and
//! the Myna page.
//!
//! The key is the desktop custom shortcut, watched live, so a rebind in the
//! desktop's settings shows up without a refresh. A proxy on
//! `com.canonical.Myna.Dictation` says whether the daemon runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib::translate::IntoGlib;
use gtk::{gdk, glib};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use myna_platform::activation::{Accelerator, Action, Activation, Conflict};
use myna_platform::Subscription;

use crate::platform::Platform;
use crate::shortcut::{
    button_action, command, default_key, ButtonAction, DefaultKey, ShortcutState,
    DEFAULT_ACCELERATOR,
};

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
/// Sets a surface's own text for a state and the refusal of the key last
/// pressed in a capture.
type Describe = Box<dyn Fn(&ShortcutState, Option<&str>)>;

/// How a surface draws the key and words its button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Onboarding's step: large key caps and a button that names the
    /// shortcut.
    Onboarding,
    /// A settings row: the key as dim text, a one-word button.
    Row,
}

/// A surface's widgets for capturing a key in place of its key caps. `stack`
/// shows "idle" (the caps and the button) or "capture". `refusal` is a room
/// showing a placeholder or its label; without one the surface's `describe`
/// shows the refusal.
pub struct InPlace {
    pub stack: gtk::Stack,
    pub field: gtk::Label,
    pub refusal: Option<(gtk::Stack, gtk::Label)>,
    pub cancel: gtk::Button,
}

/// Weak, as the button: the stack holds the button whose handler owns the
/// control.
struct InPlaceRefs {
    stack: glib::WeakRef<gtk::Stack>,
    field: glib::WeakRef<gtk::Label>,
    refusal: Option<(glib::WeakRef<gtk::Stack>, glib::WeakRef<gtk::Label>)>,
    cancel: glib::WeakRef<gtk::Button>,
}

pub struct ShortcutControl {
    keys: gtk::Box,
    /// Weak: the button's handler owns the control, and the overlay holds
    /// the button, so strong references would keep a closed window alive.
    button: glib::WeakRef<gtk::Button>,
    overlay: glib::WeakRef<adw::ToastOverlay>,
    in_place: InPlaceRefs,
    surface: Surface,
    describe: Describe,
    proxy: RefCell<Option<gio::DBusProxy>>,
    desktop: Option<Rc<dyn Activation>>,
    /// Ends the watch on the desktop's shortcut when the control goes.
    _watch: RefCell<Option<Subscription>>,
    state: RefCell<ShortcutState>,
    /// The daemon's `Toggle` does nothing, so the key pokes the socket.
    legacy: Cell<bool>,
    /// The key controller and window of a capture in place.
    capture: RefCell<Option<(gtk::Window, gtk::EventControllerKey)>>,
    /// Why the last key pressed during a capture was refused.
    refusal: RefCell<Option<String>>,
    /// The capture's key is waiting on the replace question.
    asking: Cell<bool>,
    default_pending: Cell<bool>,
    changed: RefCell<Option<Box<dyn Fn()>>>,
}

impl ShortcutControl {
    /// Drive `keys` and `button` from the desktop shortcut, capturing a new
    /// key in `in_place`. `describe` sets the surface's own text. The
    /// button's handler owns the control, so it lives as long as the button.
    pub fn attach(
        keys: gtk::Box,
        button: gtk::Button,
        overlay: adw::ToastOverlay,
        surface: Surface,
        in_place: InPlace,
        describe: Describe,
    ) -> Rc<Self> {
        let cancel = in_place.cancel.clone();
        let control = Rc::new(Self {
            keys,
            button: button.downgrade(),
            overlay: overlay.downgrade(),
            in_place: InPlaceRefs {
                stack: in_place.stack.downgrade(),
                field: in_place.field.downgrade(),
                refusal: in_place
                    .refusal
                    .map(|(room, label)| (room.downgrade(), label.downgrade())),
                cancel: in_place.cancel.downgrade(),
            },
            surface,
            describe,
            proxy: RefCell::new(None),
            desktop: Platform::current().activation(),
            _watch: RefCell::default(),
            state: RefCell::new(ShortcutState::NotRunning),
            legacy: Cell::new(false),
            capture: RefCell::default(),
            refusal: RefCell::default(),
            asking: Cell::new(false),
            default_pending: Cell::new(false),
            changed: RefCell::default(),
        });
        control.render();
        // Leaving the page, or closing its window, ends a capture.
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
        if let Some(desktop) = &control.desktop {
            let weak = Rc::downgrade(&control);
            let subscription = desktop.watch(Box::new(move || {
                if let Some(control) = weak.upgrade() {
                    control.refresh();
                }
            }));
            control._watch.replace(Some(subscription));
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

    /// A capture in place waits on the user: the step cannot finish under it.
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

    /// Install the default key once the daemon runs, unless a key is already
    /// bound or taken.
    pub fn install_default(&self) {
        self.default_pending.set(true);
        self.refresh();
    }

    fn refresh(&self) {
        let proxy = self.proxy.borrow().clone();
        let owned = proxy
            .as_ref()
            .is_some_and(|proxy| proxy.name_owner().is_some());
        self.legacy.set(
            proxy
                .as_ref()
                .is_some_and(|proxy| proxy.cached_property("Shortcut").is_some()),
        );
        let binding = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.binding().ok().flatten())
            .map(|binding| binding.to_string());
        let state = ShortcutState::observe(owned, binding.as_deref());
        self.state.replace(state.clone());
        self.render();
        // A key set up for another daemon follows this one; the write
        // comes back here as a change.
        if let (ShortcutState::Bound(binding), Some(desktop)) = (&state, &self.desktop) {
            let wanted = command(self.legacy.get());
            if desktop.command().ok().flatten().as_deref() != Some(wanted.as_str()) {
                let _ = bind(desktop.as_ref(), binding, &wanted);
            }
        }
        if self.default_pending.get() {
            let available = self
                .desktop
                .as_ref()
                .is_some_and(|desktop| conflicts(desktop.as_ref(), DEFAULT_ACCELERATOR).is_empty());
            match default_key(&state, available) {
                DefaultKey::Wait => {}
                DefaultKey::Install => {
                    self.default_pending.set(false);
                    self.install(DEFAULT_ACCELERATOR);
                }
                DefaultKey::Leave => self.default_pending.set(false),
            }
        }
    }

    fn render(&self) {
        let state = self.state.borrow().clone();
        (self.describe)(&state, self.refusal.borrow().as_deref());

        while let Some(child) = self.keys.first_child() {
            self.keys.remove(&child);
        }
        self.render_capture(matches!(state, ShortcutState::Bound(_)));
        match &state {
            ShortcutState::Bound(accelerator) => {
                self.keys.set_visible(true);
                fill_keys(&self.keys, accelerator, self.surface);
            }
            _ => self.keys.set_visible(false),
        }

        let bound = matches!(state, ShortcutState::Bound(_));
        let label = match (self.surface, bound) {
            (Surface::Onboarding, true) => gettextrs::gettext("Change shortcut"),
            (Surface::Onboarding, false) => gettextrs::gettext("Set up shortcut"),
            (Surface::Row, true) => gettextrs::gettext("Change"),
            (Surface::Row, false) => gettextrs::gettext("Set up"),
        };
        if let Some(button) = self.button.upgrade() {
            button.set_label(&label);
            button.update_property(
                &[gtk::accessible::Property::Description(&gettextrs::gettext(
                    "Press a keyboard shortcut for dictation.",
                ))],
            );
            button.set_sensitive(state != ShortcutState::NotRunning);
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

    /// Show the capture or the idle key and button, and the field's words.
    fn render_capture(&self, changing: bool) {
        let in_place = &self.in_place;
        let refusal = self.refusal.borrow();
        if let Some(stack) = in_place.stack.upgrade() {
            stack.set_visible_child_name(if self.capturing() { "capture" } else { "idle" });
        }
        if let Some(field) = in_place.field.upgrade() {
            field.set_label(&if changing {
                gettextrs::gettext("Press the new shortcut…")
            } else {
                gettextrs::gettext("Press a shortcut…")
            });
            set_class(&field, "refused", refusal.is_some());
        }
        let room = in_place.refusal.as_ref();
        if let Some((room, label)) =
            room.and_then(|(room, label)| room.upgrade().zip(label.upgrade()))
        {
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
        match button_action(&self.state.borrow(), self.capturing()) {
            ButtonAction::Nothing => {}
            ButtonAction::Capture => self.start_capture(),
            ButtonAction::CancelCapture => self.end_capture(true),
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
        // before any window sees them, so it pauses those while capturing,
        // as GNOME Settings does.
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
        if let Some(cancel) = self.in_place.cancel.upgrade() {
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
        self.claim(&accelerator, move |taken| {
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
        });
        glib::Propagation::Stop
    }

    /// Why `accelerator` cannot be taken, when the desktop reserves it.
    fn reserved(&self, accelerator: &str) -> Option<String> {
        let conflict = conflicts(self.desktop.as_deref()?, accelerator)
            .into_iter()
            .next()?;
        conflict.reserved.then(|| {
            gettextrs::gettext("{keys} is reserved for “{action}”. Press a different shortcut.")
                .replace("{keys}", &key_label(accelerator))
                .replace("{action}", &conflict.action)
        })
    }

    /// Install `accelerator`, first asking to take it from whatever desktop
    /// shortcut holds it. `then` learns whether it was installed.
    fn claim(self: &Rc<Self>, accelerator: &str, then: impl Fn(bool) + 'static) {
        let Some(conflict) = self
            .desktop
            .as_deref()
            .and_then(|desktop| conflicts(desktop, accelerator).into_iter().next())
        else {
            then(self.install(accelerator));
            return;
        };
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
    fn replace(&self, conflict: &Conflict, accelerator: &str) -> bool {
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

    /// Bind `accelerator` to the daemon's `Toggle`. Whether it was installed.
    fn install(&self, accelerator: &str) -> bool {
        let installed = self.desktop.as_ref().is_some_and(|desktop| {
            bind(desktop.as_ref(), accelerator, &command(self.legacy.get()))
        });
        if !installed {
            self.toast(adw::Toast::new(&gettextrs::gettext(
                "Could not set up the shortcut",
            )));
        }
        self.refresh();
        installed
    }

    fn toast(&self, toast: adw::Toast) {
        if let Some(overlay) = self.overlay.upgrade() {
            overlay.add_toast(toast);
        }
    }

    fn root(&self) -> Option<gtk::Root> {
        self.overlay.upgrade().and_then(|overlay| overlay.root())
    }
}

pub(crate) fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Bind `accelerator` to `command` as the dictation shortcut.
fn bind(desktop: &dyn Activation, accelerator: &str, command: &str) -> bool {
    let Ok(accelerator) = Accelerator::parse(accelerator) else {
        return false;
    };
    let action = Action {
        name: gettextrs::gettext("Dictation"),
        command: command.to_owned(),
    };
    desktop.bind(&accelerator, &action).is_ok()
}

/// What holds `accelerator` besides Myna; nothing when it is no chord.
fn conflicts(desktop: &dyn Activation, accelerator: &str) -> Vec<Conflict> {
    Accelerator::parse(accelerator)
        .ok()
        .and_then(|accelerator| desktop.conflicts(&accelerator).ok())
        .unwrap_or_default()
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
pub fn onboarding_description(state: &ShortcutState) -> String {
    match state {
        ShortcutState::Unbound => {
            gettextrs::gettext("Set up a keyboard shortcut to trigger Dictation.")
        }
        ShortcutState::Bound(_) => {
            gettextrs::gettext("You can trigger Dictation anytime by using the keyboard shortcut:")
        }
        ShortcutState::NotRunning => gettextrs::gettext(
            "Myna is not running yet. The shortcut can be set up once it starts.",
        ),
    }
}

/// The Myna page row's subtitle for `state`; the keys speak for a bound one.
pub fn row_subtitle(state: &ShortcutState) -> String {
    match state {
        ShortcutState::Bound(_) => String::new(),
        ShortcutState::Unbound => gettextrs::gettext("Not set up"),
        ShortcutState::NotRunning => gettextrs::gettext("Myna is not running"),
    }
}

/// `accelerator` drawn for `surface`, or as written when GTK cannot parse it.
pub(crate) fn fill_keys(keys: &gtk::Box, accelerator: &str, surface: Surface) {
    let Some(caps) = key_caps(accelerator) else {
        let label = gtk::Label::new(Some(accelerator));
        if surface == Surface::Row {
            label.add_css_class("dim-label");
        }
        keys.append(&label);
        return;
    };
    for (index, cap) in caps.iter().enumerate() {
        if index > 0 {
            keys.append(&gtk::Label::new(Some("+")));
        }
        let label = gtk::Label::new(Some(cap));
        label.add_css_class("keycap");
        if surface == Surface::Row {
            label.add_css_class("compact");
        }
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
