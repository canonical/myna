use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-shortcut.ui")]
    pub struct OnboardingShortcut {
        #[template_child]
        pub description: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub shortcut_box: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub shortcut_button: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingShortcut {
        const NAME: &'static str = "OnboardingShortcut";
        type Type = super::OnboardingShortcut;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingShortcut {}
    impl WidgetImpl for OnboardingShortcut {}
    impl BinImpl for OnboardingShortcut {}
}

glib::wrapper! {
    pub struct OnboardingShortcut(ObjectSubclass<imp::OnboardingShortcut>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OnboardingShortcut {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn description(&self) -> gtk::Label {
        self.imp().description.get()
    }

    pub fn shortcut_box(&self) -> gtk::Box {
        self.imp().shortcut_box.get()
    }

    pub fn shortcut_button(&self) -> gtk::Button {
        self.imp().shortcut_button.get()
    }
}

impl Default for OnboardingShortcut {
    fn default() -> Self {
        Self::new()
    }
}
