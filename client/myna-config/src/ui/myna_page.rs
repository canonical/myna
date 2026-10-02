use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/myna-page.ui")]
    pub struct MynaPage {
        #[template_child]
        pub preferences_page: gtk::TemplateChild<adw::PreferencesPage>,
        #[template_child]
        pub shortcut_group: gtk::TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub shortcut_row: gtk::TemplateChild<adw::ActionRow>,
        #[template_child]
        pub shortcut_keys: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub shortcut_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub shortcut_stack: gtk::TemplateChild<gtk::Stack>,
        #[template_child]
        pub capture_field: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub capture_cancel: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub settings_group: gtk::TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub model_group: gtk::TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub install_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub install_hint: gtk::TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MynaPage {
        const NAME: &'static str = "MynaPage";
        type Type = super::MynaPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MynaPage {}
    impl WidgetImpl for MynaPage {}
    impl NavigationPageImpl for MynaPage {}
}

glib::wrapper! {
    pub struct MynaPage(ObjectSubclass<imp::MynaPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MynaPage {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn preferences_page(&self) -> adw::PreferencesPage {
        self.imp().preferences_page.get()
    }

    pub fn settings_group(&self) -> adw::PreferencesGroup {
        self.imp().settings_group.get()
    }

    pub fn model_group(&self) -> adw::PreferencesGroup {
        self.imp().model_group.get()
    }

    pub fn install_button(&self) -> gtk::Button {
        self.imp().install_button.get()
    }

    pub fn install_hint(&self) -> gtk::Label {
        self.imp().install_hint.get()
    }

    pub fn shortcut_group(&self) -> adw::PreferencesGroup {
        self.imp().shortcut_group.get()
    }

    pub fn shortcut_row(&self) -> adw::ActionRow {
        self.imp().shortcut_row.get()
    }

    pub fn shortcut_keys(&self) -> gtk::Box {
        self.imp().shortcut_keys.get()
    }

    pub fn shortcut_button(&self) -> gtk::Button {
        self.imp().shortcut_button.get()
    }

    pub fn capture_cancel(&self) -> gtk::Button {
        self.imp().capture_cancel.get()
    }

    /// A refused key is the row's subtitle, so the capture needs no room
    /// for it.
    pub fn in_place(&self) -> crate::shortcut_ui::InPlace {
        let imp = self.imp();
        crate::shortcut_ui::InPlace {
            stack: imp.shortcut_stack.get(),
            field: imp.capture_field.get(),
            refusal: None,
            cancel: imp.capture_cancel.get(),
        }
    }
}
impl Default for MynaPage {
    fn default() -> Self {
        Self::new()
    }
}
