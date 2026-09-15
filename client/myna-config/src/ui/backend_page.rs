use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/backend-page.ui")]
    pub struct BackendPage {
        #[template_child]
        pub preferences_page: gtk::TemplateChild<adw::PreferencesPage>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BackendPage {
        const NAME: &'static str = "BackendPage";
        type Type = super::BackendPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for BackendPage {}
    impl WidgetImpl for BackendPage {}
    impl NavigationPageImpl for BackendPage {}
}

glib::wrapper! {
    pub struct BackendPage(ObjectSubclass<imp::BackendPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BackendPage {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn preferences_page(&self) -> adw::PreferencesPage {
        self.imp().preferences_page.get()
    }

    pub fn set_display_title(&self, title: &str) {
        self.set_title(title);
        self.preferences_page().set_title(title);
    }
}
impl Default for BackendPage {
    fn default() -> Self {
        Self::new()
    }
}
