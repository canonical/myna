use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-welcome.ui")]
    pub struct OnboardingWelcome {
        #[template_child]
        pub status: gtk::TemplateChild<adw::StatusPage>,
        #[template_child]
        pub start_button: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingWelcome {
        const NAME: &'static str = "OnboardingWelcome";
        type Type = super::OnboardingWelcome;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingWelcome {}
    impl WidgetImpl for OnboardingWelcome {}
    impl BinImpl for OnboardingWelcome {}
}

glib::wrapper! {
    pub struct OnboardingWelcome(ObjectSubclass<imp::OnboardingWelcome>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OnboardingWelcome {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn status(&self) -> adw::StatusPage {
        self.imp().status.get()
    }

    pub fn start_button(&self) -> gtk::Button {
        self.imp().start_button.get()
    }
}

impl Default for OnboardingWelcome {
    fn default() -> Self {
        Self::new()
    }
}
