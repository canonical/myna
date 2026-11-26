use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::prelude::*;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/install-control.ui")]
    pub struct InstallControl {
        #[template_child]
        pub button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub installing: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub spinner: gtk::TemplateChild<gtk::Spinner>,
        #[template_child]
        pub progress: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub installed: gtk::TemplateChild<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InstallControl {
        const NAME: &'static str = "InstallControl";
        type Type = super::InstallControl;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InstallControl {}
    impl WidgetImpl for InstallControl {}
    impl BoxImpl for InstallControl {}
}

glib::wrapper! {
    /// A row's install suffix, alike wherever Myna Settings installs: an
    /// Install button, then a spinner with how far snapd has come, then an
    /// "Installed" check.
    pub struct InstallControl(ObjectSubclass<imp::InstallControl>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

/// A spinner and how far the install or enable has come.
#[derive(Clone)]
pub struct RowProgress {
    pub container: gtk::Box,
    pub spinner: gtk::Spinner,
    pub label: gtk::Label,
}

impl InstallControl {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn button(&self) -> gtk::Button {
        self.imp().button.get()
    }

    pub fn installed(&self) -> gtk::Box {
        self.imp().installed.get()
    }

    pub fn progress(&self) -> RowProgress {
        let imp = self.imp();
        RowProgress {
            container: imp.installing.get(),
            spinner: imp.spinner.get(),
            label: imp.progress.get(),
        }
    }

    /// The button, named `accessible` to assistive technology, since rows
    /// side by side would all read "Install".
    pub fn show_offer(&self, label: &str, accessible: &str) {
        self.show(Some(label), None, false);
        let button = self.button();
        // A button is labelled by its label child, over any name set.
        button.reset_relation(gtk::AccessibleRelation::LabelledBy);
        button.update_property(&[gtk::accessible::Property::Label(accessible)]);
    }

    /// The spinner with snapd's download `percent`, while one is known.
    pub fn show_installing(&self, percent: Option<u8>) {
        self.show(None, Some(&installing_text(percent)), false);
    }

    pub fn show_installed(&self) {
        self.show(None, None, true);
    }

    fn show(&self, button: Option<&str>, busy: Option<&str>, installed: bool) {
        let imp = self.imp();
        imp.button.set_visible(button.is_some());
        if let Some(label) = button {
            imp.button.set_label(label);
        }
        imp.installing.set_visible(busy.is_some());
        imp.spinner.set_spinning(busy.is_some());
        if let Some(text) = busy {
            imp.progress.set_label(text);
        }
        imp.installed.set_visible(installed);
    }
}

impl Default for InstallControl {
    fn default() -> Self {
        Self::new()
    }
}

/// An install as it reads beside the spinner.
pub fn installing_text(percent: Option<u8>) -> String {
    match percent {
        Some(percent) => {
            // TRANSLATORS: {percent} is how much of the download has arrived, a number from 0 to 100.
            let frame = gettextrs::gettext("Installing {percent}%");
            frame.replace("{percent}", &percent.to_string())
        }
        None => gettextrs::gettext("Installing…"),
    }
}
