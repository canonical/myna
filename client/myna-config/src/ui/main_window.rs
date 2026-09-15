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
        pub view_stack: gtk::TemplateChild<adw::ViewStack>,
        #[template_child]
        pub general_nav: gtk::TemplateChild<adw::NavigationView>,
        #[template_child]
        pub backend_nav: gtk::TemplateChild<adw::NavigationView>,
        #[template_child]
        pub diagnostics_nav: gtk::TemplateChild<adw::NavigationView>,
        #[template_child]
        pub view_switcher_bar: gtk::TemplateChild<adw::ViewSwitcherBar>,
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

    pub fn view_stack(&self) -> adw::ViewStack {
        self.imp().view_stack.get()
    }

    pub fn general_nav(&self) -> adw::NavigationView {
        self.imp().general_nav.get()
    }

    pub fn backend_nav(&self) -> adw::NavigationView {
        self.imp().backend_nav.get()
    }

    pub fn diagnostics_nav(&self) -> adw::NavigationView {
        self.imp().diagnostics_nav.get()
    }

    pub fn view_switcher_bar(&self) -> adw::ViewSwitcherBar {
        self.imp().view_switcher_bar.get()
    }
}
