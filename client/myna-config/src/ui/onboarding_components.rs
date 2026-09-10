use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-components.ui")]
    pub struct OnboardingComponents {
        #[template_child]
        pub subtitle: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub list: gtk::TemplateChild<gtk::ListBox>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingComponents {
        const NAME: &'static str = "OnboardingComponents";
        type Type = super::OnboardingComponents;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingComponents {}
    impl WidgetImpl for OnboardingComponents {}
    impl BinImpl for OnboardingComponents {}
}

glib::wrapper! {
    pub struct OnboardingComponents(ObjectSubclass<imp::OnboardingComponents>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OnboardingComponents {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn subtitle(&self) -> gtk::Label {
        self.imp().subtitle.get()
    }

    pub fn list(&self) -> gtk::ListBox {
        self.imp().list.get()
    }
}

impl Default for OnboardingComponents {
    fn default() -> Self {
        Self::new()
    }
}
