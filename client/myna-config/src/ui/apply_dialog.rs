use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/apply-dialog.ui")]
    pub struct ApplyDialog {
        #[template_child]
        pub confirmation_label: gtk::TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ApplyDialog {
        const NAME: &'static str = "ApplyDialog";
        type Type = super::ApplyDialog;
        type ParentType = adw::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ApplyDialog {}
    impl WidgetImpl for ApplyDialog {}
    impl AdwDialogImpl for ApplyDialog {}
    impl AdwAlertDialogImpl for ApplyDialog {}
}

glib::wrapper! {
    pub struct ApplyDialog(ObjectSubclass<imp::ApplyDialog>)
        @extends gtk::Widget, adw::Dialog, adw::AlertDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ApplyDialog {
    pub fn new(confirmation: &str) -> Self {
        super::register_resources();
        let dialog: Self = glib::Object::builder().build();
        dialog.imp().confirmation_label.set_label(confirmation);
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        dialog
    }
}
