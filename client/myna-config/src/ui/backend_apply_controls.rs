use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/backend-apply-controls.ui")]
    pub struct BackendApplyControls {
        #[template_child]
        pub progress_spinner: gtk::TemplateChild<gtk::Spinner>,
        #[template_child]
        pub button_box: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub revert_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub apply_button: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BackendApplyControls {
        const NAME: &'static str = "BackendApplyControls";
        type Type = super::BackendApplyControls;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for BackendApplyControls {}
    impl WidgetImpl for BackendApplyControls {}
    impl ListBoxRowImpl for BackendApplyControls {}
    impl PreferencesRowImpl for BackendApplyControls {}
    impl ActionRowImpl for BackendApplyControls {}
}

glib::wrapper! {
    pub struct BackendApplyControls(ObjectSubclass<imp::BackendApplyControls>)
        @extends gtk::Widget, gtk::ListBoxRow, adw::PreferencesRow, adw::ActionRow,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl BackendApplyControls {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn progress_spinner(&self) -> gtk::Spinner {
        self.imp().progress_spinner.get()
    }

    pub fn button_box(&self) -> gtk::Box {
        self.imp().button_box.get()
    }

    pub fn revert_button(&self) -> gtk::Button {
        self.imp().revert_button.get()
    }

    pub fn apply_button(&self) -> gtk::Button {
        self.imp().apply_button.get()
    }
}

impl Default for BackendApplyControls {
    fn default() -> Self {
        Self::new()
    }
}
