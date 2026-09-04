use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/active-backend-dialog.ui")]
    pub struct ActiveBackendDialog {
        #[template_child]
        pub switch_confirmation_label: gtk::TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ActiveBackendDialog {
        const NAME: &'static str = "ActiveBackendDialog";
        type Type = super::ActiveBackendDialog;
        type ParentType = adw::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ActiveBackendDialog {}
    impl WidgetImpl for ActiveBackendDialog {}
    impl AdwDialogImpl for ActiveBackendDialog {}
    impl AdwAlertDialogImpl for ActiveBackendDialog {}
}

glib::wrapper! {
    pub struct ActiveBackendDialog(ObjectSubclass<imp::ActiveBackendDialog>)
        @extends gtk::Widget, adw::Dialog, adw::AlertDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ActiveBackendDialog {
    pub fn new(confirmation: &str) -> Self {
        super::register_resources();
        let dialog: Self = glib::Object::builder().build();
        dialog
            .imp()
            .switch_confirmation_label
            .set_label(confirmation);
        dialog.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
        dialog
    }
}
