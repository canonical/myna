use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/sidebar-row.ui")]
    pub struct SidebarRow {
        #[template_child]
        pub icon: gtk::TemplateChild<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SidebarRow {
        const NAME: &'static str = "SidebarRow";
        type Type = super::SidebarRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SidebarRow {}
    impl WidgetImpl for SidebarRow {}
    impl ListBoxRowImpl for SidebarRow {}
    impl PreferencesRowImpl for SidebarRow {}
    impl ActionRowImpl for SidebarRow {}
}

glib::wrapper! {
    pub struct SidebarRow(ObjectSubclass<imp::SidebarRow>)
        @extends gtk::Widget, gtk::ListBoxRow, adw::PreferencesRow, adw::ActionRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl SidebarRow {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn set_icon_name(&self, icon_name: &str) {
        self.imp().icon.set_icon_name(Some(icon_name));
    }

    pub fn icon(&self) -> gtk::Image {
        self.imp().icon.get()
    }
}
impl Default for SidebarRow {
    fn default() -> Self {
        Self::new()
    }
}
