use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{gio, glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/main-window.ui")]
    pub struct MainWindow {
        #[template_child]
        pub overlay: gtk::TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: gtk::TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub sidebar_list: gtk::TemplateChild<gtk::ListBox>,
        #[template_child]
        pub myna_row: gtk::TemplateChild<adw::ActionRow>,
        #[template_child]
        pub diagnostics_row: gtk::TemplateChild<adw::ActionRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MainWindow {
        const NAME: &'static str = "MainWindow";
        type Type = super::MainWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MainWindow {}
    impl WidgetImpl for MainWindow {}
    impl WindowImpl for MainWindow {}
    impl ApplicationWindowImpl for MainWindow {}
    impl AdwApplicationWindowImpl for MainWindow {}
}

glib::wrapper! {
    pub struct MainWindow(ObjectSubclass<imp::MainWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl MainWindow {
    pub fn new(application: &adw::Application) -> Self {
        super::register_resources();
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    pub fn overlay(&self) -> adw::ToastOverlay {
        self.imp().overlay.get()
    }

    pub fn split_view(&self) -> adw::NavigationSplitView {
        self.imp().split_view.get()
    }

    pub fn sidebar_list(&self) -> gtk::ListBox {
        self.imp().sidebar_list.get()
    }

    pub fn myna_row(&self) -> adw::ActionRow {
        self.imp().myna_row.get()
    }

    pub fn diagnostics_row(&self) -> adw::ActionRow {
        self.imp().diagnostics_row.get()
    }
}
