use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-components.ui")]
    pub struct OnboardingComponents {
        #[template_child]
        pub description: gtk::TemplateChild<crate::ui::BalancedLabel>,
        #[template_child]
        pub install_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub install_labels: gtk::TemplateChild<gtk::Stack>,
        #[template_child]
        pub status: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub status_spinner: gtk::TemplateChild<gtk::Spinner>,
        #[template_child]
        pub status_warning: gtk::TemplateChild<gtk::Image>,
        #[template_child]
        pub status_label: gtk::TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingComponents {
        const NAME: &'static str = "OnboardingComponents";
        type Type = super::OnboardingComponents;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            <crate::ui::BalancedLabel as glib::prelude::StaticTypeExt>::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingComponents {}
    impl WidgetImpl for OnboardingComponents {}
    impl BinImpl for OnboardingComponents {}
}

glib::wrapper! {
    pub struct OnboardingComponents(ObjectSubclass<imp::OnboardingComponents>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// What the line under the button shows.
pub enum ComponentsStatus<'a> {
    Hidden,
    /// Dimmed text, such as the download size.
    Note(&'a str),
    /// A spinner beside what is under way.
    Busy(&'a str),
    Warning(&'a str),
}

impl OnboardingComponents {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    /// The line under the title.
    pub fn description(&self) -> gtk::Label {
        self.imp().description.text_label()
    }

    pub fn install_button(&self) -> gtk::Button {
        self.imp().install_button.get()
    }

    /// The button's label as shown: its child is a stack of every label, so
    /// it keeps one width.
    pub fn install_label(&self) -> String {
        self.imp()
            .install_labels
            .visible_child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
            .map(|label| label.label().to_string())
            .unwrap_or_default()
    }

    /// Show the button's `offer`, `installing` or `installed` label.
    pub fn show_install_label(&self, name: &str) {
        let imp = self.imp();
        imp.install_labels.set_visible_child_name(name);
        let label = self.install_label();
        imp.install_button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    pub fn status(&self) -> gtk::Box {
        self.imp().status.get()
    }

    pub fn status_spinner(&self) -> gtk::Spinner {
        self.imp().status_spinner.get()
    }

    pub fn status_warning(&self) -> gtk::Image {
        self.imp().status_warning.get()
    }

    pub fn status_label(&self) -> gtk::Label {
        self.imp().status_label.get()
    }

    pub fn show_status(&self, status: ComponentsStatus) {
        let imp = self.imp();
        let (text, busy, warning) = match status {
            ComponentsStatus::Hidden => ("", false, false),
            ComponentsStatus::Note(text) => (text, false, false),
            ComponentsStatus::Busy(text) => (text, true, false),
            ComponentsStatus::Warning(text) => (text, false, true),
        };
        // The line keeps its room when empty, so the centred column does not
        // jump as it comes and goes.
        imp.status_spinner.set_visible(busy);
        imp.status_spinner.set_spinning(busy);
        imp.status_warning.set_visible(warning);
        imp.status_label.set_label(text);
        let note = !busy && !warning;
        if note != imp.status_label.has_css_class("dim-label") {
            if note {
                imp.status_label.add_css_class("dim-label");
            } else {
                imp.status_label.remove_css_class("dim-label");
            }
        }
        imp.status
            .update_state(&[gtk::accessible::State::Busy(busy)]);
    }
}

impl Default for OnboardingComponents {
    fn default() -> Self {
        Self::new()
    }
}
