use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-window.ui")]
    pub struct OnboardingWindow {
        #[template_child]
        pub overlay: gtk::TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub window_title: gtk::TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub stack: gtk::TemplateChild<gtk::Stack>,
        #[template_child]
        pub back_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub forward_button: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingWindow {
        const NAME: &'static str = "OnboardingWindow";
        type Type = super::OnboardingWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingWindow {}
    impl WidgetImpl for OnboardingWindow {}
    impl WindowImpl for OnboardingWindow {}
    impl ApplicationWindowImpl for OnboardingWindow {}
    impl AdwApplicationWindowImpl for OnboardingWindow {}
}

glib::wrapper! {
    pub struct OnboardingWindow(ObjectSubclass<imp::OnboardingWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                    gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

impl OnboardingWindow {
    pub fn new(application: &adw::Application) -> Self {
        super::register_resources();
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    pub fn overlay(&self) -> adw::ToastOverlay {
        self.imp().overlay.get()
    }

    pub fn window_title(&self) -> adw::WindowTitle {
        self.imp().window_title.get()
    }

    pub fn stack(&self) -> gtk::Stack {
        self.imp().stack.get()
    }

    pub fn back_button(&self) -> gtk::Button {
        self.imp().back_button.get()
    }

    pub fn forward_button(&self) -> gtk::Button {
        self.imp().forward_button.get()
    }
}
