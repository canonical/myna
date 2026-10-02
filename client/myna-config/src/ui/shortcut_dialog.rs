use std::cell::RefCell;

use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{gdk, glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

/// Takes a captured accelerator, or refuses it with the reason to show.
type Captured = Box<dyn Fn(&str) -> Option<String>>;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/shortcut-dialog.ui")]
    pub struct ShortcutDialog {
        #[template_child]
        pub example: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub refusal: gtk::TemplateChild<gtk::Label>,
        pub captured: RefCell<Option<Captured>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShortcutDialog {
        const NAME: &'static str = "ShortcutDialog";
        type Type = super::ShortcutDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ShortcutDialog {
        fn constructed(&self) {
            self.parent_constructed();
            crate::shortcut_ui::fill_keys(
                &self.example,
                crate::shortcut::DEFAULT_ACCELERATOR,
                Some(crate::shortcut::DEFAULT_ACCELERATOR),
                crate::shortcut_ui::Surface::Onboarding,
            );
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let dialog = self.obj().downgrade();
            keys.connect_key_pressed(move |_, key, _, state| {
                dialog
                    .upgrade()
                    .map_or(glib::Propagation::Proceed, |dialog| {
                        dialog.press(key, state)
                    })
            });
            self.obj().add_controller(keys);
            // The desktop grabs the keys it uses (Super+L, the Calculator
            // key) before any window sees them, so it pauses those while
            // capturing, as GNOME Settings does.
            self.obj().connect_map(|dialog| {
                if let Some(toplevel) = toplevel(dialog) {
                    toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
                }
            });
            self.obj().connect_unmap(|dialog| {
                if let Some(toplevel) = toplevel(dialog) {
                    toplevel.restore_system_shortcuts();
                }
            });
        }
    }

    fn toplevel(dialog: &super::ShortcutDialog) -> Option<gdk::Toplevel> {
        dialog.root()?.surface()?.downcast::<gdk::Toplevel>().ok()
    }
    impl WidgetImpl for ShortcutDialog {}
    impl AdwDialogImpl for ShortcutDialog {}
}

glib::wrapper! {
    /// Captures the key combination for the desktop dictation shortcut.
    pub struct ShortcutDialog(ObjectSubclass<imp::ShortcutDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ShortcutDialog {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    /// Run `captured` with the accelerator the user presses. A reason it
    /// returns keeps the dialog open and is shown there.
    pub fn connect_captured(&self, captured: impl Fn(&str) -> Option<String> + 'static) {
        self.imp().captured.replace(Some(Box::new(captured)));
    }

    /// Escape cancels. A key [`crate::shortcut_ui::capture_key`] takes goes
    /// to the captured handler, which closes the dialog unless it refuses it.
    pub fn press(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        use crate::shortcut_ui::{capture_key, Capture};
        let accelerator = match capture_key(key, state) {
            Capture::Cancel => {
                self.close();
                return glib::Propagation::Stop;
            }
            Capture::Ignore => return glib::Propagation::Proceed,
            Capture::Take(accelerator) => accelerator,
        };
        let refusal = self
            .imp()
            .captured
            .borrow()
            .as_ref()
            .and_then(|captured| captured(&accelerator));
        match refusal {
            Some(reason) => {
                let label = self.imp().refusal.get();
                label.set_label(&reason);
                label.set_visible(true);
            }
            None => {
                self.close();
            }
        }
        glib::Propagation::Stop
    }

    /// The reason the last key was refused, while it is shown.
    pub fn refusal(&self) -> Option<String> {
        let label = self.imp().refusal.get();
        label.is_visible().then(|| label.label().to_string())
    }
}

impl Default for ShortcutDialog {
    fn default() -> Self {
        Self::new()
    }
}
