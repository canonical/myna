use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/operation-error-dialog.ui")]
    pub struct OperationErrorDialog {
        #[template_child]
        pub details_label: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub copy_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub scroller: gtk::TemplateChild<gtk::ScrolledWindow>,
        pub details_text: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OperationErrorDialog {
        const NAME: &'static str = "OperationErrorDialog";
        type Type = super::OperationErrorDialog;
        type ParentType = adw::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OperationErrorDialog {}
    impl WidgetImpl for OperationErrorDialog {}
    impl AdwDialogImpl for OperationErrorDialog {}
    impl AdwAlertDialogImpl for OperationErrorDialog {}
}

glib::wrapper! {
    /// Reusable presenter for full operation error details.
    ///
    /// * Body wraps and is selectable so users can read every character.
    /// * `Copy Details` puts the full plain text on the clipboard.
    /// * The dialog itself only displays plain text (`use-markup: false`) so
    ///   backend-provided content can never inject Pango markup.
    pub struct OperationErrorDialog(ObjectSubclass<imp::OperationErrorDialog>)
        @extends gtk::Widget, adw::Dialog, adw::AlertDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OperationErrorDialog {
    /// Build a dialog with the given heading, one-line summary, and full
    /// detail text. All strings must already be redacted by the caller with
    /// [`crate::diagnostics::redact_text`] where necessary; the dialog does not
    /// redact.
    pub fn new(heading: &str, summary: &str, details: &str) -> Self {
        super::register_resources();
        let dialog: Self = glib::Object::builder().build();
        dialog.set_heading(Some(heading));
        dialog.set_body(summary);
        let imp = dialog.imp();
        imp.details_label.set_text(details);
        imp.details_text.replace(details.to_owned());
        let dialog_weak = dialog.downgrade();
        imp.copy_button.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let text = dialog.imp().details_text.borrow().clone();
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&text);
            }
        });
        dialog
    }

    pub fn details_text(&self) -> String {
        self.imp().details_text.borrow().clone()
    }
}
