use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/status-page.ui")]
    pub struct StatusPage {
        #[template_child]
        pub status: gtk::TemplateChild<adw::StatusPage>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StatusPage {
        const NAME: &'static str = "StatusPage";
        type Type = super::StatusPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for StatusPage {}
    impl WidgetImpl for StatusPage {}
    impl NavigationPageImpl for StatusPage {}
}

glib::wrapper! {
    pub struct StatusPage(ObjectSubclass<imp::StatusPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StatusPage {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn set_status(&self, title: &str, description: &str, icon_name: &str) {
        self.set_title(title);
        let status = self.imp().status.get();
        status.set_title(title);
        status.set_description(Some(&crate::markup::escape_markup(description)));
        status.set_icon_name(Some(icon_name));
    }

    pub fn status(&self) -> adw::StatusPage {
        self.imp().status.get()
    }
}
impl Default for StatusPage {
    fn default() -> Self {
        Self::new()
    }
}
