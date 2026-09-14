use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/diagnostics-page.ui")]
    pub struct DiagnosticsPage {
        #[template_child]
        pub preferences_page: gtk::TemplateChild<adw::PreferencesPage>,
        #[template_child]
        pub warnings_group: gtk::TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub report_group: gtk::TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub report_view: gtk::TemplateChild<gtk::TextView>,
        #[template_child]
        pub copy_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub refresh_button: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DiagnosticsPage {
        const NAME: &'static str = "DiagnosticsPage";
        type Type = super::DiagnosticsPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DiagnosticsPage {}
    impl WidgetImpl for DiagnosticsPage {}
    impl NavigationPageImpl for DiagnosticsPage {}
}

glib::wrapper! {
    pub struct DiagnosticsPage(ObjectSubclass<imp::DiagnosticsPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DiagnosticsPage {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn preferences_page(&self) -> adw::PreferencesPage {
        self.imp().preferences_page.get()
    }

    pub fn warnings_group(&self) -> adw::PreferencesGroup {
        self.imp().warnings_group.get()
    }

    pub fn report_group(&self) -> adw::PreferencesGroup {
        self.imp().report_group.get()
    }

    pub fn report_view(&self) -> gtk::TextView {
        self.imp().report_view.get()
    }

    pub fn copy_button(&self) -> gtk::Button {
        self.imp().copy_button.get()
    }

    pub fn refresh_button(&self) -> gtk::Button {
        self.imp().refresh_button.get()
    }
}
impl Default for DiagnosticsPage {
    fn default() -> Self {
        Self::new()
    }
}
