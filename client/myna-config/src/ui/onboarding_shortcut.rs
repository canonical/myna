use adw::subclass::prelude::*;
use glib::prelude::*;
use glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, CompositeTemplate};
use gtk4 as gtk;
use libadwaita as adw;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/com/canonical/Myna/Config/ui/onboarding-shortcut.ui")]
    pub struct OnboardingShortcut {
        #[template_child]
        pub description: gtk::TemplateChild<crate::ui::BalancedLabel>,
        #[template_child]
        pub shortcut_box: gtk::TemplateChild<gtk::Box>,
        #[template_child]
        pub shortcut_button: gtk::TemplateChild<gtk::Button>,
        #[template_child]
        pub shortcut_stack: gtk::TemplateChild<gtk::Stack>,
        #[template_child]
        pub capture_field: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub capture_illustration: gtk::TemplateChild<gtk::Picture>,
        #[template_child]
        pub capture_room: gtk::TemplateChild<gtk::Stack>,
        #[template_child]
        pub capture_refusal: gtk::TemplateChild<gtk::Label>,
        #[template_child]
        pub capture_cancel: gtk::TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingShortcut {
        const NAME: &'static str = "OnboardingShortcut";
        type Type = super::OnboardingShortcut;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            <crate::ui::BalancedLabel as glib::prelude::StaticTypeExt>::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingShortcut {
        fn constructed(&self) {
            self.parent_constructed();
            let style = adw::StyleManager::default();
            let picture = self.capture_illustration.get().downgrade();
            let follow = move |style: &adw::StyleManager| {
                if let Some(picture) = picture.upgrade() {
                    picture.set_resource(Some(illustration(style.is_dark())));
                }
            };
            follow(&style);
            style.connect_dark_notify(follow);
        }
    }
    impl WidgetImpl for OnboardingShortcut {}
    impl BinImpl for OnboardingShortcut {}
}

/// GNOME Settings' keyboard, drawn in the foreground on the window colour.
fn illustration(dark: bool) -> &'static str {
    if dark {
        "/com/canonical/Myna/Config/ui/enter-shortcut-dark.svg"
    } else {
        "/com/canonical/Myna/Config/ui/enter-shortcut.svg"
    }
}

glib::wrapper! {
    pub struct OnboardingShortcut(ObjectSubclass<imp::OnboardingShortcut>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OnboardingShortcut {
    pub fn new() -> Self {
        super::register_resources();
        glib::Object::builder().build()
    }

    pub fn description(&self) -> gtk::Label {
        self.imp().description.text_label()
    }

    pub fn shortcut_box(&self) -> gtk::Box {
        self.imp().shortcut_box.get()
    }

    pub fn shortcut_button(&self) -> gtk::Button {
        self.imp().shortcut_button.get()
    }

    pub fn in_place(&self) -> crate::shortcut_ui::InPlace {
        let imp = self.imp();
        crate::shortcut_ui::InPlace {
            stack: imp.shortcut_stack.get(),
            field: imp.capture_field.get(),
            refusal: Some((imp.capture_room.get(), imp.capture_refusal.get())),
            cancel: imp.capture_cancel.get(),
        }
    }
}

impl Default for OnboardingShortcut {
    fn default() -> Self {
        Self::new()
    }
}
