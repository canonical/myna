use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::adapters::client_settings::GioClientSettings;
use crate::domain::ClientSettingValue;
use crate::myna_settings::{
    choice_display_label, widget_plan, DebouncedTextCommit, MynaSettingsController, PageState,
    PersistenceRequest, PersistenceWriter, Placement, SettingRow, SettingsEvent, WidgetKind,
};
use crate::onboarding::needs_onboarding;
use crate::ports::{ClientSettings, ClientSettingsError};
use crate::ui;
use crate::APP_ID;

const TEMPLATE_ENV: &str = "MYNA_CONFIG_TEMPLATE_TEST";
const ACCESSIBILITY_ENV: &str = "MYNA_CONFIG_ACCESSIBILITY_TEST";
const TYPING_ENV: &str = "MYNA_CONFIG_TYPING_TEST";
const ONBOARDING_ENV: &str = "MYNA_CONFIG_ONBOARDING_TEST";
const SHORTCUT_ENV: &str = "MYNA_CONFIG_SHORTCUT_TEST";
const SHORTCUT_CONTROL_ENV: &str = "MYNA_CONFIG_SHORTCUT_CONTROL_TEST";
const ONBOARDING_CONTROL_ENV: &str = "MYNA_CONFIG_ONBOARDING_CONTROL_TEST";
const BACKENDS_ENV: &str = "MYNA_CONFIG_BACKENDS_TEST";
const ICON_RESOURCES: &str = "/com/canonical/Myna/Config/icons";
/// The probes must never claim the real application id: registering it while a
/// Myna Settings is already running takes the remote-instance path, and
/// `gtk_window_set_application` then segfaults against an application that was
/// never started.
const PROBE_APP_ID: &str = "com.canonical.Myna.Config.Probe";

/// One id per probe *process*: the probes run in parallel under one `cargo
/// test`, and two of them sharing an id is the same remote-instance hazard
/// described above, with the same segfault.
fn probe_app_id() -> String {
    format!("{PROBE_APP_ID}.p{}", std::process::id())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearancePolicy {
    pub reduced_motion: bool,
    pub high_contrast: bool,
}

pub const fn appearance_policy(animations_enabled: bool, high_contrast: bool) -> AppearancePolicy {
    AppearancePolicy {
        reduced_motion: !animations_enabled,
        high_contrast,
    }
}

pub fn run() -> glib::ExitCode {
    if smoke_requested(std::env::var_os(TEMPLATE_ENV).as_deref()) {
        return template_probe();
    }

    if smoke_requested(std::env::var_os(ACCESSIBILITY_ENV).as_deref()) {
        return accessibility_probe();
    }

    if smoke_requested(std::env::var_os(TYPING_ENV).as_deref()) {
        return typing_probe();
    }

    if smoke_requested(std::env::var_os(ONBOARDING_ENV).as_deref()) {
        return onboarding_probe();
    }

    if smoke_requested(std::env::var_os(ONBOARDING_CONTROL_ENV).as_deref()) {
        return onboarding_control_probe();
    }

    if smoke_requested(std::env::var_os(SHORTCUT_ENV).as_deref()) {
        return shortcut_probe(false);
    }

    if smoke_requested(std::env::var_os(SHORTCUT_CONTROL_ENV).as_deref()) {
        return shortcut_probe(true);
    }

    if smoke_requested(std::env::var_os(BACKENDS_ENV).as_deref()) {
        return backends_probe();
    }

    ui::register_resources();
    let application = new_application(APP_ID);
    application.connect_activate(build_window);
    application.run_with_args::<&str>(&[])
}

fn new_application(application_id: &str) -> adw::Application {
    let application = adw::Application::builder()
        .application_id(application_id)
        .build();
    application.set_accels_for_action("window.close", &["<Control>w"]);
    // `Application::quit` destroys windows without their close requests,
    // which is where an in-flight operation is abandoned.
    let quit = gio::ActionEntry::builder("quit")
        .activate(|application: &adw::Application, _, _| {
            for window in application.windows() {
                window.close();
            }
        })
        .build();
    let about = gio::ActionEntry::builder("about")
        .activate(|application: &adw::Application, _, _| present_about(application))
        .build();
    application.add_action_entries([quit, about]);
    application.set_accels_for_action("app.quit", &["<Control>q"]);
    application.set_accels_for_action("win.refresh", &["<Control>r"]);
    application
}

fn present_about(application: &adw::Application) {
    let dialog = adw::AboutDialog::builder()
        .application_name(gettextrs::gettext("Myna Settings"))
        .application_icon(APP_ID)
        .developer_name("Canonical")
        .version(env!("MYNA_VERSION"))
        .website("https://github.com/canonical/myna")
        .copyright("© 2025-2026 Canonical Ltd.")
        .license_type(gtk::License::Agpl30)
        // Translators: your name, one translator per line.
        .translator_credits(gettextrs::gettext("translator-credits"))
        .build();
    dialog.present(application.active_window().as_ref());
}

fn smoke_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn accessibility_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config accessibility probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }

    let application = new_application(&probe_app_id());
    let _ = application.register(None::<&gio::Cancellable>);

    let window = ui::MainWindow::new(&application);
    let diagnostics_nav = window.diagnostics_nav();
    let view_stack = window.view_stack();

    let diagnostics = ui::DiagnosticsPage::new();
    let copy_button = diagnostics.copy_button();
    let refresh_button = diagnostics.refresh_button();
    let template = gio::resources_lookup_data(
        "/com/canonical/Myna/Config/ui/diagnostics-page.ui",
        gio::ResourceLookupFlags::NONE,
    )
    .ok()
    .and_then(|bytes| String::from_utf8(bytes.as_ref().to_vec()).ok())
    .unwrap_or_default()
    // blueprint-compiler 0.12 (Noble) spells it "true"; GtkBuilder reads both.
    .replace(r#"translatable="true""#, r#"translatable="yes""#);
    if [
        r#"<property name="label" translatable="yes">Refresh diagnostics</property>"#,
        r#"<property name="label" translatable="yes">Copy diagnostics</property>"#,
        r#"<property name="label" translatable="yes">Diagnostic report</property>"#,
        r#"<property name="description" translatable="yes">Check this computer, Myna and every model again.</property>"#,
    ]
    .iter()
    .any(|metadata| !template.contains(metadata))
    {
        eprintln!("compiled diagnostics template is missing accessibility metadata");
        return glib::ExitCode::FAILURE;
    }
    println!("template-metadata: verified");

    diagnostics_nav.replace(&[diagnostics.clone().upcast()]);
    view_stack.set_visible_child_name("diagnostics");
    install_appearance_policy(window.upcast_ref());
    window.present();
    settle_gtk();

    if !refresh_button.grab_focus()
        || gtk::prelude::GtkWindowExt::focus(&window).as_ref() != Some(refresh_button.upcast_ref())
        || !diagnostics.child_focus(gtk::DirectionType::TabForward)
    {
        eprintln!("diagnostics controls are not keyboard traversable");
        return glib::ExitCode::FAILURE;
    }
    settle_gtk();
    let focus = gtk::prelude::GtkWindowExt::focus(&window);
    if focus.as_ref() != Some(copy_button.upcast_ref())
        && focus.as_ref() != Some(diagnostics.report_view().upcast_ref())
    {
        eprintln!("tab traversal did not reach another diagnostics control");
        return glib::ExitCode::FAILURE;
    }
    println!("keyboard-traversal: verified");

    window.set_default_size(500, 500);
    settle_gtk();
    if !window.view_switcher_bar().reveals() {
        eprintln!("view switcher did not collapse below the 600sp breakpoint");
        return glib::ExitCode::FAILURE;
    }
    println!("narrow-layout: collapsed");

    let accessibility_settings = gio::Settings::new("org.gnome.desktop.a11y.interface");
    let original_high_contrast = accessibility_settings.boolean("high-contrast");
    if accessibility_settings
        .set_boolean("high-contrast", true)
        .is_err()
    {
        eprintln!("could not enable the high-contrast test preference");
        return glib::ExitCode::FAILURE;
    }
    settle_gtk();
    if !current_appearance_policy().high_contrast || !window.has_css_class("high-contrast") {
        eprintln!(
            "high-contrast preference did not update the production window (setting={}, style={}, class={})",
            accessibility_settings.boolean("high-contrast"),
            adw::StyleManager::default().is_high_contrast(),
            window.has_css_class("high-contrast")
        );
        let _ = accessibility_settings.set_boolean("high-contrast", original_high_contrast);
        return glib::ExitCode::FAILURE;
    }
    println!("high-contrast: verified");
    if accessibility_settings
        .set_boolean("high-contrast", original_high_contrast)
        .is_err()
    {
        eprintln!("could not restore the high-contrast test preference");
        return glib::ExitCode::FAILURE;
    }
    settle_gtk();

    let settings = gtk::Settings::default().expect("GTK settings");
    let original_animations = settings.is_gtk_enable_animations();
    settings.set_gtk_enable_animations(true);
    settle_gtk();
    settings.set_gtk_enable_animations(false);
    settle_gtk();
    if !window.has_css_class("reduced-motion") {
        eprintln!("reduced-motion preference did not update the production window");
        settings.set_gtk_enable_animations(original_animations);
        return glib::ExitCode::FAILURE;
    }
    println!("reduced-motion: verified");
    settings.set_gtk_enable_animations(original_animations);
    settle_gtk();

    let policy = current_appearance_policy();
    if window.has_css_class("reduced-motion") != policy.reduced_motion
        || window.has_css_class("high-contrast") != policy.high_contrast
        || adw::StyleManager::default().color_scheme() != adw::ColorScheme::Default
    {
        eprintln!("system appearance policy was not applied to the production window");
        return glib::ExitCode::FAILURE;
    }
    println!("appearance-policy: applied");

    let menu = find_descendant(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::MenuButton>()
            .is_some_and(|button| button.is_primary())
    })
    .and_then(|widget| widget.downcast::<gtk::MenuButton>().ok())
    .and_then(|button| button.menu_model());
    let menu_actions: Vec<String> = menu.map(|menu| menu_actions(&menu)).unwrap_or_default();
    if menu_actions != ["win.setup", "app.about"] {
        eprintln!("the main menu offers {menu_actions:?}");
        return glib::ExitCode::FAILURE;
    }
    application.activate_action("about", None);
    settle_gtk();
    let Some(about) = window
        .visible_dialog()
        .and_then(|dialog| dialog.downcast::<adw::AboutDialog>().ok())
        .filter(|about| {
            about.version() == env!("MYNA_VERSION") && about.application_icon() == APP_ID
        })
    else {
        eprintln!("About did not open over the window with this version and icon");
        return glib::ExitCode::FAILURE;
    };
    about.close();
    settle_gtk();
    println!("main-menu: setup and about");

    for (accelerator, action) in [("<Control>w", "window.close"), ("<Control>q", "app.quit")] {
        if !application
            .actions_for_accel(accelerator)
            .iter()
            .any(|bound| bound == action)
        {
            eprintln!("{accelerator} does not activate {action}");
            return glib::ExitCode::FAILURE;
        }
    }
    println!("close-accelerator: bound");
    let shut_down = Rc::new(Cell::new(false));
    window.connect_close_request({
        let shut_down = shut_down.clone();
        move |_| {
            shut_down.set(true);
            glib::Propagation::Proceed
        }
    });
    application.activate_action("quit", None);
    settle_gtk();
    if !shut_down.get() || !application.windows().is_empty() {
        eprintln!("quitting did not close the window through its close request");
        return glib::ExitCode::FAILURE;
    }
    println!("quit-accelerator: closes windows");

    glib::ExitCode::SUCCESS
}

/// Walk the onboarding wizard by activating its buttons, holding nothing but
/// the widgets - exactly what production does.
///
/// The regression this exists for: `present` returned the only strong
/// reference to the controller, the caller dropped it, and every button was
/// left upgrading a dead weak reference. Everything rendered and nothing
/// worked, so the probe must assert on widget state after dropping that
/// reference, never through the controller it just released.
fn onboarding_probe() -> glib::ExitCode {
    use crate::onboarding::{assess, Machine};
    use crate::onboarding_ui::OnboardingUi;

    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config onboarding probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    let application = new_application(&probe_app_id());
    let _ = application.register(None::<&gio::Cancellable>);

    let step = |window: &ui::OnboardingWindow| {
        window
            .navigation()
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    };

    // A machine with nothing installed: the flow opens, and its component step
    // refuses to advance.
    // Every command fails, so a refresh on focus still sees a bare machine.
    let runner = std::sync::Arc::new(crate::command::FakeCommandRunner::default());
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                runner.clone(),
            )),
            Rc::new(ProbeMachine::new()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.window()
    };
    settle_gtk();
    if step(&window) != "welcome" {
        eprintln!("the wizard did not open on its first step");
        return glib::ExitCode::FAILURE;
    }
    // Headless runs have no hicolor copy, so the icon must come from the
    // application's own resources.
    if !gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&window)).has_icon(APP_ID) {
        eprintln!("the icon theme does not find the application icon");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-icon: themed");
    // Not by name: a stale icon cache listing a deleted hicolor copy hid it.
    let icon = find_descendant(window.upcast_ref(), &|widget| {
        widget.downcast_ref::<gtk::Image>().is_some_and(|image| {
            image.is_mapped()
                && image.storage_type() == gtk::ImageType::Paintable
                && image.resource().as_deref()
                    == Some("/com/canonical/Myna/Config/icons/scalable/apps/com.canonical.Myna.Config.svg")
                && image
                    .paintable()
                    .is_some_and(|icon| icon.intrinsic_width() > 0)
                && image.pixel_size() == 96
        })
    });
    if icon.is_none() {
        eprintln!("the welcome step shows no 96 px application icon");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-welcome: icon shown");
    if !one_line_each(
        &window,
        &[
            "Set up Dictation",
            "Dictation runs locally, with no data leaving your computer.",
        ],
    ) {
        eprintln!("the welcome step wraps a line that fits the window");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-wrap: welcome on one line each");

    let forward = window.forward_button();
    if !forward.is_mapped()
        || !forward.is_sensitive()
        || forward.label().as_deref() != Some(gettextrs::gettext("Next").as_str())
        || !standard_button(&forward, true)
    {
        eprintln!("the welcome step offers no suggested Next in the footer");
        return glib::ExitCode::FAILURE;
    }
    if window.default_width() != 800 || window.default_height() != 600 {
        eprintln!("the wizard does not open at the design's 800x600");
        return glib::ExitCode::FAILURE;
    }
    if header(&window) != Some(false) {
        eprintln!("the welcome step's header is not an untitled bar without a back arrow");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-chrome: welcome untitled, no back");
    forward.emit_clicked();
    settle_gtk();
    if step(&window) != "components" {
        eprintln!("Next on the welcome step did not reach the component step");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-start: advanced");

    // GTK 4.14 let an unscrolled page grow past the window, out of this bar.
    let in_view = forward.compute_bounds(&window).is_some_and(|bounds| {
        bounds.y() >= 0.0
            && bounds.y() + bounds.height() <= window.height() as f32
            && bounds.x() + bounds.width() == window.width() as f32 - 24.0
    });
    if !in_view {
        eprintln!("the forward button lies outside the window");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-layout: forward in view");
    if header(&window) != Some(true) || !standard_button(&forward, false) {
        eprintln!("the component step's header is not an untitled bar with a back arrow");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-chrome: components untitled, back");
    if forward.is_sensitive() {
        eprintln!("the component step offered to advance with required components missing");
        return glib::ExitCode::FAILURE;
    }
    forward.emit_clicked();
    settle_gtk();
    if step(&window) != "components" {
        eprintln!("an insensitive forward button still advanced the flow");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-gate: held");
    if !one_line_each(
        &window,
        &[
            "Install components",
            "You need to install some components for Dictation to work.",
        ],
    ) {
        eprintln!("the component step wraps a line that fits the window");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-wrap: components on one line each");
    if installed_status(&window).is_some() {
        eprintln!("the footer claims everything is installed on a bare machine");
        return glib::ExitCode::FAILURE;
    }
    if !components_headed(&window, false) {
        eprintln!("the component step is not headed as the design");
        return glib::ExitCode::FAILURE;
    }
    // Nothing to paste: every component installs from its own row.
    let pasted = find_descendant(window.upcast_ref(), &|widget| {
        widget.is_mapped()
            && (widget.has_css_class("monospace")
                || widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    button.icon_name().as_deref() == Some("edit-copy-symbolic")
                }))
    });
    if pasted.is_some() {
        eprintln!("the component step still shows commands to paste");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-commands: none");
    let Some(page) = components_page(&window) else {
        eprintln!("the component step shows no component page");
        return glib::ExitCode::FAILURE;
    };
    let flag = page.flag_switch();
    if !flag.is_mapped()
        || flag.is_active()
        || !flag.is_sensitive()
        || !flag
            .ancestor(gtk::ListBox::static_type())
            .is_some_and(|list| list.has_css_class("boxed-list"))
        || page.flag_row().title() != gettextrs::gettext("Let Myna run in the background")
        || page.flag_row().subtitle().as_deref()
            != Some(
                gettextrs::gettext("Dictation needs it. You may be asked for your password.")
                    .as_str(),
            )
    {
        eprintln!("the component step offers no boxed switch for the flag");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-flag: a switch, off");
    let list = page.component_list();
    let titles: Vec<String> = [
        crate::onboarding::ComponentId::Myna,
        crate::onboarding::ComponentId::Model,
        crate::onboarding::ComponentId::ShellExtension,
    ]
    .into_iter()
    .filter_map(|id| page.row(id))
    .filter(|row| row.row.parent().as_ref() == Some(list.upcast_ref()))
    .map(|row| row.row.title().to_string())
    .collect();
    if titles != ["Dictation app", "Speech-to-text model", "Shell extension"]
        || !list.has_css_class("boxed-list")
    {
        eprintln!("the component step lists {titles:?}");
        return glib::ExitCode::FAILURE;
    }
    // The initial assessment found no extension: nothing to press there.
    if list.is_sensitive() || rows_offer(&page) != ["Install", "Install", "-"] {
        eprintln!(
            "without the flag the list is not locked on two Install buttons: {:?}",
            rows_offer(&page)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-rows: locked until the flag");

    // Installing happens in another window; coming back re-reads the machine.
    let elsewhere = gtk::Window::new();
    elsewhere.present();
    settle_gtk();
    elsewhere.close();
    window.present();
    let refreshed = || !runner.calls().is_empty();
    for _ in 0..100 {
        if refreshed() {
            break;
        }
        settle_gtk();
    }
    if !refreshed() {
        eprintln!("regaining focus on the component step did not re-read the machine");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-refresh: re-read on focus");
    // Every read fails on this machine, so the step says why rather than
    // only that both components are missing.
    let unreadable = || {
        let status = window.setup_status();
        status.is_mapped() && status.label().starts_with("Setup status unavailable.")
    };
    for _ in 0..100 {
        if unreadable() {
            break;
        }
        settle_gtk();
    }
    if !unreadable() {
        eprintln!(
            "the component step hid that it cannot read the machine: {:?}",
            window.setup_status().label()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-unreadable: said why");
    window.close();
    settle_gtk();

    if let Err(failure) = probe_flag_switch(&application) {
        eprintln!("{failure}");
        return glib::ExitCode::FAILURE;
    }

    if let Err(failure) = probe_installs(&application) {
        eprintln!("{failure}");
        return glib::ExitCode::FAILURE;
    }

    if let Err(failure) = probe_extension_enable(&application) {
        eprintln!("{failure}");
        return glib::ExitCode::FAILURE;
    }

    // With the flag on, the list takes input: each snap offers Install, and
    // a disabled extension Enable.
    let machine = ProbeMachine::flagged();
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine {
                user_daemons: true,
                extension: crate::onboarding::ExtensionState::Disabled,
                ..Machine::default()
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Disabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let Some(page) = components_page(&window) else {
        eprintln!("the component step shows no component page");
        return glib::ExitCode::FAILURE;
    };
    let buttons: Vec<bool> = [
        crate::onboarding::ComponentId::Myna,
        crate::onboarding::ComponentId::Model,
        crate::onboarding::ComponentId::ShellExtension,
    ]
    .into_iter()
    .filter_map(|id| page.row(id))
    .map(|row| row.button.is_sensitive() && row.row.is_sensitive())
    .collect();
    if !page.flag_switch().state()
        || !page.component_list().is_sensitive()
        || rows_offer(&page) != ["Install", "Install", "Enable"]
        || buttons != [true, true, true]
        || window.forward_button().is_sensitive()
    {
        eprintln!(
            "with the flag on the rows offer {:?}, sensitive {buttons:?}",
            rows_offer(&page)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-rows: unlocked by the flag");
    window.close();
    settle_gtk();

    // Components installed while the step shows are found without the
    // window ever losing focus. Finding the last one sets dictation up once,
    // says so, and moves on by itself.
    let machine = ProbeMachine::bare();
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.set_beat(Duration::from_millis(300));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    machine.hold_restart(true);
    machine.hold_changes(true);
    machine.install();
    for _ in 0..100 {
        if setup_spinner(&window) {
            break;
        }
        settle_gtk();
    }
    if !setup_spinner(&window) || step(&window) != "components" {
        eprintln!("components installed while the step showed set nothing up");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-poll: found without focus");
    // snapd shows the backend connected before its install has mounted it
    // into Myna: restarting then leaves the daemon without it.
    let polled = machine.change_reads();
    for _ in 0..100 {
        if machine.change_reads() >= polled + 2 {
            break;
        }
        settle_gtk();
    }
    if machine.change_reads() < polled + 2
        || machine.restarts_attempted() != 0
        || !setup_spinner(&window)
    {
        eprintln!("setup restarted Myna while snapd was still installing the model");
        return glib::ExitCode::FAILURE;
    }
    let status = window.setup_status();
    if !status.is_mapped()
        || !status.label().starts_with("Downloading myna-parakeet: ")
        || !status
            .label()
            .ends_with(glib::format_size(734_003_200).as_str())
    {
        eprintln!(
            "the footer did not say what setup waits on: {:?}",
            status.label()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-status: the download shown");
    machine.hold_changes(false);
    println!("onboarding-snapd: waits for the install to finish");
    // Next is the manual path; it must not start a second setup.
    window.forward_button().emit_clicked();
    machine.hold_restart(false);
    let shown = || installed_status(&window) == Some(true) && !setup_spinner(&window);
    for _ in 0..100 {
        if shown() || step(&window) != "components" {
            break;
        }
        settle_gtk();
    }
    if !shown() || step(&window) != "components" {
        eprintln!("the footer did not say everything is installed before moving on");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto: status before advancing");
    let reached = |window: &ui::OnboardingWindow, name: &str| {
        for _ in 0..100 {
            if step(window) == name {
                return true;
            }
            settle_gtk();
        }
        false
    };
    if !reached(&window, "shortcut") {
        eprintln!("the wizard did not move on after setting dictation up");
        return glib::ExitCode::FAILURE;
    }
    if machine.applied() != [vec!["restart-myna".to_owned()]] {
        eprintln!("automatic setup did not run once: {:?}", machine.applied());
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto: set up once and advanced");
    let reads = machine.reads();
    for _ in 0..5 {
        settle_gtk();
    }
    if machine.reads() != reads {
        eprintln!("the wizard kept polling after leaving the component step");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-poll: stopped once found");
    window.close();
    settle_gtk();

    // Closing the wizard while snapd still installs leaves the machine alone
    // once it finishes.
    let machine = ProbeMachine::bare();
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    machine.hold_changes(true);
    machine.install();
    for _ in 0..100 {
        if machine.change_reads() > 0 {
            break;
        }
        settle_gtk();
    }
    window.close();
    machine.hold_changes(false);
    let reads = machine.change_reads();
    for _ in 0..20 {
        settle_gtk();
    }
    if reads == 0 || machine.change_reads() != reads || machine.restarts_attempted() != 0 {
        eprintln!("setup went on after the wizard closed");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-close: setup stopped with the wizard");

    // Next while the status shows moves on at once, without setting up again.
    let machine = ProbeMachine::bare();
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.set_beat(Duration::from_secs(60));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    machine.install();
    for _ in 0..100 {
        if installed_status(&window) == Some(true) {
            break;
        }
        settle_gtk();
    }
    window.forward_button().emit_clicked();
    settle_gtk();
    if step(&window) != "shortcut" || machine.applied().len() != 1 {
        eprintln!(
            "Next after automatic setup reached {} having applied {:?}",
            step(&window),
            machine.applied()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto: Next skips the pause");
    window.close();
    settle_gtk();

    // Without the extension the required components alone never move on by
    // themselves: Next does.
    let machine = ProbeMachine::bare();
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Unavailable),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.set_beat(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    machine.install();
    for _ in 0..100 {
        if installed_status(&window) == Some(true) {
            break;
        }
        settle_gtk();
    }
    for _ in 0..20 {
        settle_gtk();
    }
    if installed_status(&window) != Some(true)
        || step(&window) != "components"
        || !window.forward_button().is_sensitive()
        || machine.restarts_attempted() != 0
    {
        eprintln!(
            "the required components alone reached {} having restarted {} times",
            step(&window),
            machine.restarts_attempted()
        );
        return glib::ExitCode::FAILURE;
    }
    window.forward_button().emit_clicked();
    for _ in 0..100 {
        if step(&window) == "shortcut" {
            break;
        }
        settle_gtk();
    }
    if step(&window) != "shortcut" {
        eprintln!("Next without the extension stayed on {}", step(&window));
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-optional: the extension waits for Next");
    if !window.navigation().visible_page().is_some_and(|page| {
        page.can_pop() && WidgetExt::activate_action(&page, "navigation.pop", None).is_ok()
    }) {
        eprintln!("the shortcut step could not go back");
        return glib::ExitCode::FAILURE;
    }
    settle_gtk();
    let Some(page) = components_page(&window) else {
        eprintln!("going back did not show the component page");
        return glib::ExitCode::FAILURE;
    };
    let extension = page
        .row(crate::onboarding::ComponentId::ShellExtension)
        .map(|row| row.row);
    let fallback = gettextrs::gettext(
        "Not available on this system. Dictation still works and shows its status in notifications.",
    );
    if rows_offer(&page) != ["Installed", "Installed", "-"]
        || !page.component_list().is_sensitive()
        // Insensitive would dim the explanation past reading.
        || extension.as_ref().is_none_or(|row| {
            !row.is_sensitive() || row.subtitle().as_deref() != Some(fallback.as_str())
        })
    {
        eprintln!(
            "an unavailable extension shows {:?}, subtitled {:?}",
            rows_offer(&page),
            extension.and_then(|row| row.subtitle())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-rows: an unavailable extension says it falls back");
    window.close();
    settle_gtk();

    // Each reason the extension cannot run tells the user what to do.
    let machine = ProbeMachine::bare();
    let extensions = ProbeExtensions::new(crate::onboarding::ExtensionState::NeedsRelogin);
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            extensions.clone(),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    for (state, expected) in [
        (
            crate::onboarding::ExtensionState::NeedsRelogin,
            gettextrs::gettext(
                "Log out and back in to use it. Until then, Dictation shows its status in notifications.",
            ),
        ),
        (
            crate::onboarding::ExtensionState::ShadowedByUserCopy,
            gettextrs::gettext(
                "Hidden by a copy in your home folder. Remove it, then log out and back in.",
            ),
        ),
        (
            crate::onboarding::ExtensionState::TurnedOff,
            gettextrs::gettext(
                "Extensions are turned off. Turn them on in the Extensions app to use it. Until then, Dictation shows its status in notifications.",
            ),
        ),
        (
            crate::onboarding::ExtensionState::Failed,
            gettextrs::gettext(
                "Failed to start. Dictation still works and shows its status in notifications.",
            ),
        ),
        (
            crate::onboarding::ExtensionState::OutOfDate,
            gettextrs::gettext(
                "Does not work with this version of GNOME. Dictation still works and shows its status in notifications.",
            ),
        ),
        (
            crate::onboarding::ExtensionState::Locked,
            gettextrs::gettext(
                "Turned off by your administrator. Dictation still works and shows its status in notifications.",
            ),
        ),
    ] {
        extensions.state.set(state);
        let subtitle = || {
            components_page(&window)
                .and_then(|page| page.row(crate::onboarding::ComponentId::ShellExtension))
                .and_then(|row| row.row.subtitle())
        };
        for _ in 0..100 {
            if subtitle().as_deref() == Some(expected.as_str()) {
                break;
            }
            settle_gtk();
        }
        if subtitle().as_deref() != Some(expected.as_str()) {
            eprintln!("an extension {state:?} is subtitled {:?}", subtitle());
            return glib::ExitCode::FAILURE;
        }
    }
    println!("onboarding-rows: an extension that cannot run says why");
    window.close();
    settle_gtk();

    // A failed automatic setup reports itself in a toast, not a dialog to
    // dismiss, stays, and Next retries.
    let machine = ProbeMachine::bare();
    machine.refuse_restarts(1);
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine::default()),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    machine.install();
    let failed = || setup_failure_toasted(&window);
    for _ in 0..100 {
        if failed() {
            break;
        }
        settle_gtk();
    }
    if !failed()
        || window.visible_dialog().is_some()
        || step(&window) != "components"
        || setup_spinner(&window)
        || !window.forward_button().is_sensitive()
    {
        eprintln!(
            "a failed automatic setup showed toasts {:?} and a dialog {}, not Next",
            toast_texts(&window),
            window.visible_dialog().is_some()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto-failure: reported");
    // The toast times out; the footer keeps saying dictation is not set up.
    if !setup_failed_shown(&window) || installed_status(&window).is_some() {
        eprintln!(
            "after a failed setup the footer read installed {:?}, not set up {}",
            installed_status(&window),
            setup_failed_shown(&window)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto-failure: the footer says so");
    match setup_failure_report(&window) {
        // The report names the command, as the Settings window's do.
        Some(report) if report.contains(PROBE_RESTART_FAILURE) && report.contains("systemctl") => {}
        report => {
            eprintln!("the failed setup's Details read {report:?}");
            return glib::ExitCode::FAILURE;
        }
    }
    println!("onboarding-auto-failure: Details name the cause");
    window.forward_button().emit_clicked();
    if !reached(&window, "shortcut") || machine.applied() != [vec!["restart-myna".to_owned()]] {
        eprintln!("Next did not retry setup: {:?}", machine.applied());
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-auto-failure: Next retries");
    window.close();
    settle_gtk();

    // A machine whose snaps cannot be read fails to finish setting up, says
    // so, and stays on the component step.
    let installed = [crate::diagnostics::InstalledSnap {
        name: crate::onboarding::MYNA_SNAP.to_owned(),
        version: "1".to_owned(),
    }];
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine {
                user_daemons: true,
                ..Machine::new(&installed, 1)
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(crate::command::FakeCommandRunner::default()),
            )),
            Rc::new(ProbeMachine::new()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    window.forward_button().emit_clicked();
    let failed = || setup_failure_toasted(&window);
    for _ in 0..100 {
        if failed() {
            break;
        }
        settle_gtk();
    }
    if !failed() || step(&window) != "components" {
        eprintln!("a failed setup did not report itself on the component step");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup-failure: reported");
    window.close();
    settle_gtk();

    // A model the store did not connect costs a polkit prompt on Next.
    // Dismissing it is the user's answer, not a failure: nothing to report,
    // and Next asks again. A refusal is reported.
    let installed = [crate::diagnostics::InstalledSnap {
        name: crate::onboarding::MYNA_SNAP.to_owned(),
        version: "1".to_owned(),
    }];
    let machine = ProbeMachine::new();
    machine.disconnect_backends();
    machine.dismiss_authorization(true);
    let window = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine {
                user_daemons: true,
                extension: crate::onboarding::ExtensionState::Enabled,
                ..Machine::new(&installed, 1)
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    window.forward_button().emit_clicked();
    for _ in 0..100 {
        if machine.switches_attempted() == 1 && window.forward_button().is_sensitive() {
            break;
        }
        settle_gtk();
    }
    for _ in 0..5 {
        settle_gtk();
    }
    if machine.switches_attempted() != 1
        || !toast_texts(&window).is_empty()
        || window.visible_dialog().is_some()
        || step(&window) != "components"
        || setup_spinner(&window)
        || !window.forward_button().is_sensitive()
    {
        eprintln!(
            "a dismissed connection prompt left step {} after {} switches, toasts {:?}, a dialog {}",
            step(&window),
            machine.switches_attempted(),
            toast_texts(&window),
            window.visible_dialog().is_some()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-connect: a dismissed prompt stays silently");
    machine.dismiss_authorization(false);
    machine.deny_switches(true);
    window.forward_button().emit_clicked();
    for _ in 0..100 {
        if setup_failure_toasted(&window) {
            break;
        }
        settle_gtk();
    }
    if !setup_failure_toasted(&window)
        || machine.switches_attempted() != 2
        || step(&window) != "components"
    {
        eprintln!(
            "a refused connection left step {} after {} switches, toasts {:?}",
            step(&window),
            machine.switches_attempted(),
            toast_texts(&window)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-connect: a refusal is reported");
    machine.deny_switches(false);
    window.forward_button().emit_clicked();
    if !reached(&window, "shortcut") || machine.switches_attempted() != 3 {
        eprintln!(
            "Next did not connect the model on the third try: {}",
            machine.switches_attempted()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-connect: Next connects the model");
    window.close();
    settle_gtk();

    // A machine with everything installed walks to the end, and Done
    // closes the application.
    let installed = [crate::diagnostics::InstalledSnap {
        name: crate::onboarding::MYNA_SNAP.to_owned(),
        version: "1".to_owned(),
    }];
    let machine = ProbeMachine::new();
    let (window, shortcut_button) = {
        let ui = OnboardingUi::present_with_ports(
            &application,
            assess(Machine {
                user_daemons: true,
                extension: crate::onboarding::ExtensionState::Enabled,
                ..Machine::new(&installed, 1)
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
            crate::onboarding_ui::Opener::FirstRun,
        );
        (ui.window(), ui.shortcut_button())
    };
    settle_gtk();
    let forward = window.forward_button();
    if installed_status(&window).is_some() {
        eprintln!("the footer status shows outside the component step");
        return glib::ExitCode::FAILURE;
    }
    forward.emit_clicked();
    settle_gtk();
    if !forward.is_sensitive() || !standard_button(&forward, true) {
        eprintln!("the component step refused to advance with everything installed");
        return glib::ExitCode::FAILURE;
    }
    if installed_status(&window) != Some(true) {
        eprintln!("the footer does not say every component is installed, left of Next");
        return glib::ExitCode::FAILURE;
    }
    if !components_headed(&window, true) {
        eprintln!("the component step still asks for components once everything is installed");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-installed: shown in the footer");
    let Some(page) = components_page(&window) else {
        eprintln!("the component step shows no component page");
        return glib::ExitCode::FAILURE;
    };
    if rows_offer(&page) != ["Installed", "Installed", "Enabled"] || !page.flag_switch().state() {
        eprintln!("an installed machine's rows show {:?}", rows_offer(&page));
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-rows: each installed");
    let reaches = |name: &str| {
        for _ in 0..100 {
            if step(&window) == name {
                return true;
            }
            settle_gtk();
        }
        false
    };
    let can_pop = || {
        window
            .navigation()
            .visible_page()
            .is_some_and(|page| page.can_pop())
    };
    machine.hold_restart(true);
    forward.emit_clicked();
    // Hiding the back arrow for a quick setup flashed it.
    if !can_pop() {
        eprintln!("the back arrow went a moment into setting up");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: back stays for a moment");
    settle_gtk();
    // A spinner that flashes for a quick setup reads as a glitch: for a
    // moment the footer keeps saying everything is installed.
    if setup_spinner(&window) || installed_status(&window) != Some(true) || forward.is_sensitive() {
        eprintln!(
            "a moment into setting up the footer showed a spinner: {}, the status: {:?}",
            setup_spinner(&window),
            installed_status(&window)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: no spinner for a moment");
    for _ in 0..100 {
        if setup_spinner(&window) {
            break;
        }
        settle_gtk();
    }
    if !setup_spinner(&window) || installed_status(&window).is_some() {
        eprintln!("setting up showed no spinner in the footer's status");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: spinner while setting up");
    if can_pop() {
        eprintln!("the component step could be left while its spinner showed");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: no leaving past the moment");
    machine.hold_restart(false);
    if !reaches("shortcut") {
        eprintln!("the component step did not reach the shortcut step");
        return glib::ExitCode::FAILURE;
    }
    // The fixture's backend is already connected, so leaving the step only
    // restarts the daemon.
    if machine.applied() != [vec!["restart-myna".to_owned()]] {
        eprintln!(
            "leaving the component step did not restart the daemon: {:?}",
            machine.applied()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: restarted the daemon");
    // The header bar's back button, Escape and Alt+Left all pop.
    let popped = window.navigation().visible_page().is_some_and(|page| {
        page.can_pop() && WidgetExt::activate_action(&page, "navigation.pop", None).is_ok()
    });
    settle_gtk();
    if !popped || step(&window) != "components" {
        eprintln!("going back did not return to the component step");
        return glib::ExitCode::FAILURE;
    }
    // Back during that moment lets the setup finish without moving on.
    machine.hold_restart(true);
    forward.emit_clicked();
    let popped = window.navigation().visible_page().is_some_and(|page| {
        page.can_pop() && WidgetExt::activate_action(&page, "navigation.pop", None).is_ok()
    });
    settle_gtk();
    machine.hold_restart(false);
    for _ in 0..100 {
        if machine.applied().len() == 2 {
            break;
        }
        settle_gtk();
    }
    for _ in 0..20 {
        settle_gtk();
    }
    if !popped || machine.applied().len() != 2 || step(&window) != "welcome" {
        eprintln!(
            "back during setup: popped {popped}, restarts {:?}, on {}",
            machine.applied(),
            step(&window)
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-setup: back stays back");
    forward.emit_clicked();
    if !reaches("components") {
        eprintln!("Next on the welcome step did not return to the components");
        return glib::ExitCode::FAILURE;
    }
    forward.emit_clicked();
    if !reaches("shortcut") {
        eprintln!("returning to the component step stranded the flow there");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-walk: reached the last step");
    if !shortcut_headed(&window) {
        eprintln!("the shortcut step is not headed as the design");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-shortcut: headed as the design");
    if !one_line_each(&window, &["How to dictate"]) {
        eprintln!("the shortcut step wraps its title");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-wrap: shortcut title on one line");
    if installed_status(&window).is_some() || setup_spinner(&window) {
        eprintln!("the footer status stayed on the last step");
        return glib::ExitCode::FAILURE;
    }

    // No daemon runs under the probe, and nothing can bind a key without one.
    if shortcut_button.is_sensitive() {
        eprintln!("the shortcut step offered to bind a key with no daemon to bind it");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-shortcut: waits for the daemon");
    if !standard_button(&shortcut_button, false) {
        eprintln!("the shortcut step's button is not a plain Adwaita button");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-shortcut: button plain");
    if forward.label().as_deref() != Some(gettextrs::gettext("Done").as_str())
        || !standard_button(&forward, true)
    {
        eprintln!("the last step does not finish with a suggested Done");
        return glib::ExitCode::FAILURE;
    }
    if header(&window) != Some(true) {
        eprintln!("the shortcut step's header is not an untitled bar with a back arrow");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-chrome: shortcut untitled, back");

    // Opened from the settings window, Done closes only the wizard.
    let settings = gtk::ApplicationWindow::new(&application);
    settings.present();
    let over_settings = OnboardingUi::present_with_ports(
        &application,
        assess(Machine {
            user_daemons: true,
            extension: crate::onboarding::ExtensionState::Enabled,
            ..Machine::new(&installed, 1)
        }),
        Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(machine.clone()),
        )),
        Rc::new(machine.clone()),
        ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
        crate::onboarding_ui::Opener::Settings(settings.upcast_ref()),
    )
    .window();
    settle_gtk();
    over_settings.navigation().push_by_tag("shortcut");
    settle_gtk();
    over_settings.forward_button().emit_clicked();
    settle_gtk();
    let open = application.windows();
    if open
        .iter()
        .any(|open| open == over_settings.upcast_ref::<gtk::Window>())
        || !open
            .iter()
            .any(|open| open == settings.upcast_ref::<gtk::Window>())
        || window.application().is_none()
    {
        eprintln!(
            "Done over the settings window left {} windows open",
            open.len()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-finish: Done over Settings keeps Settings");

    // First run, with no settings window behind it, Done closes Myna
    // Settings, whatever else it had open.
    forward.emit_clicked();
    settle_gtk();
    let open = application.windows();
    if !open.is_empty() {
        eprintln!("Done left {} windows open", open.len());
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-finish: Done closes Myna Settings");
    glib::ExitCode::SUCCESS
}

/// With no key the caps leave no room behind: the sentence leads straight to
/// the button, and the column grows by exactly the caps once a key lands.
fn shortcut_page_closes_up(page: &ui::OnboardingShortcut) -> Result<(), String> {
    use crate::shortcut::{ShortcutPath, ShortcutState};
    use crate::shortcut_ui::{fill_keys, onboarding_description, Surface};
    let keys = page.shortcut_box();
    let column = std::iter::successors(Some(keys.clone().upcast::<gtk::Widget>()), |w| w.parent())
        .find(|w| w.parent().is_some_and(|p| p.is::<adw::Clamp>()))
        .ok_or("the key caps sit in no clamped column")?;
    let height = |state: &ShortcutState| {
        while let Some(child) = keys.first_child() {
            keys.remove(&child);
        }
        if let ShortcutState::Bound(trigger) = state {
            fill_keys(&keys, trigger, Surface::Onboarding);
        }
        keys.set_visible(matches!(state, ShortcutState::Bound(_)));
        column.measure(gtk::Orientation::Vertical, 540).1
    };
    // One sentence for both, so only the caps differ.
    let bound = ShortcutState::Bound("<Super>j".to_owned());
    page.description()
        .set_label(&onboarding_description(&bound, ShortcutPath::Portal));
    let bound = height(&bound);
    let caps = keys.measure(gtk::Orientation::Vertical, -1).1;
    let unbound = height(&ShortcutState::Unbound);
    if bound - unbound < caps {
        return Err(format!(
            "the shortcut step's column is {unbound} px high unbound and {bound} px bound, \
             with {caps} px of key caps"
        ));
    }
    Ok(())
}

fn template_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config template probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }

    let application = new_application(&probe_app_id());
    let _ = application.register(None::<&gio::Cancellable>);
    for resource in [
        "backend-page.ui",
        "diagnostics-page.ui",
        "install-models-dialog.ui",
        "main-window.ui",
        "myna-page.ui",
        "onboarding-components.ui",
        "onboarding-shortcut.ui",
        "onboarding-welcome.ui",
        "onboarding-window.ui",
        "operation-error-dialog.ui",
        "shortcut-dialog.ui",
        "status-page.ui",
    ] {
        let path = format!("/com/canonical/Myna/Config/ui/{resource}");
        if let Err(error) = gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE) {
            eprintln!("missing template resource {path}: {error}");
            return glib::ExitCode::FAILURE;
        }
    }

    let window = ui::MainWindow::new(&application);
    let _ = (
        window.overlay(),
        window.view_stack(),
        window.general_nav(),
        window.backend_nav(),
        window.diagnostics_nav(),
    );
    println!("MainWindow");
    let myna = ui::MynaPage::new();
    let _ = (
        myna.preferences_page(),
        myna.settings_group(),
        myna.model_group(),
        myna.install_button(),
        myna.shortcut_group(),
        myna.shortcut_row(),
        myna.shortcut_keys(),
        myna.shortcut_button(),
    );
    println!("MynaPage");
    let backend = ui::BackendPage::new();
    let _ = backend.preferences_page();
    backend.set_display_title("Backend");
    if backend.title() != "Backend" || backend.preferences_page().title() != "Backend" {
        eprintln!("backend template did not propagate its navigation title");
        return glib::ExitCode::FAILURE;
    }
    println!("BackendPage");
    let diagnostics = ui::DiagnosticsPage::new();
    let _ = (
        diagnostics.preferences_page(),
        diagnostics.report_group(),
        diagnostics.report_view(),
        diagnostics.copy_button(),
        diagnostics.refresh_button(),
    );
    println!("DiagnosticsPage");
    ui::OnboardingWelcome::new();
    println!("OnboardingWelcome");
    let components = ui::OnboardingComponents::new();
    let _ = (
        components.flag_row(),
        components.flag_switch(),
        components.component_list(),
        components.row(crate::onboarding::ComponentId::ShellExtension),
    );
    println!("OnboardingComponents");
    let shortcut = ui::OnboardingShortcut::new();
    let _ = (
        shortcut.description(),
        shortcut.shortcut_box(),
        shortcut.shortcut_button(),
    );
    println!("OnboardingShortcut");
    if let Err(error) = shortcut_page_closes_up(&shortcut) {
        eprintln!("{error}");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-shortcut: no room held for absent key caps");
    let onboarding = ui::OnboardingWindow::new(&application);
    let _ = (
        onboarding.overlay(),
        onboarding.navigation(),
        onboarding.installed_status(),
        onboarding.forward_button(),
    );
    println!("OnboardingWindow");
    let status = ui::StatusPage::new();
    let _ = status.status();
    println!("StatusPage");
    let _ = ui::ShortcutDialog::new();
    println!("ShortcutDialog");
    let _ = ui::InstallModelsDialog::new().families();
    println!("InstallModelsDialog");
    let error_dialog = ui::OperationErrorDialog::new(
        "Operation failed",
        "concise summary",
        "full <safe> details & example",
    );
    // Round-trip the details text to prove the template accepted the plain
    // string with markup characters intact and without warnings.
    if error_dialog.details_text() != "full <safe> details & example" {
        eprintln!("operation error dialog did not preserve details text");
        return glib::ExitCode::FAILURE;
    }
    // A snap plug name wraps whole, never after its hyphen.
    let request = "Request: POST /v2/interfaces (connect myna:backend myna-parakeet:provider)";
    let wrapped = ui::OperationErrorDialog::new("Operation failed", "summary", request);
    let label = wrapped.details_label();
    let layout = label.create_pango_layout(Some(&label.text()));
    let head = label.create_pango_layout(Some(
        "Request: POST /v2/interfaces (connect myna:backend myna-",
    ));
    layout.set_wrap(gtk::pango::WrapMode::WordChar);
    layout.set_width(head.size().0 + gtk::pango::SCALE);
    let first_line = layout
        .line(0)
        .map(|line| {
            let start = line.start_index() as usize;
            let text = layout.text();
            text[start..start + line.length() as usize].to_owned()
        })
        .unwrap_or_default();
    if first_line.trim_end().ends_with('-') || wrapped.details_text() != request {
        eprintln!("the details wrap a plug name at its hyphen: {first_line:?}");
        return glib::ExitCode::FAILURE;
    }
    println!("OperationErrorDialog");
    glib::ExitCode::SUCCESS
}

/// Read the machine once, then open either the onboarding wizard or the
/// settings window. The read is the same two subprocesses a startup refresh
/// already budgets for (`snap list`, `snap connections`), and the wizard is
/// handed the result rather than repeating it.
fn build_window(application: &adw::Application) {
    ui::register_resources();
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }

    gtk::Window::set_default_icon_name(APP_ID);
    let application = application.clone();
    // Nothing is on screen while the machine is read, and a GApplication with
    // no window and no held use count quits the moment `activate` returns.
    let hold = application.hold();
    glib::spawn_future_local(async move {
        let runner = std::sync::Arc::new(crate::command::GioCommandRunner);
        let components = crate::onboarding_ui::assess_machine(
            &crate::adapters::snap_backend::SnapBackendRepository::new(runner.clone()),
            &crate::adapters::system_configurator::PkexecSystemConfigurator::new(runner),
            &crate::adapters::shell_extensions::GnomeShellExtensions::new(),
        )
        .await;
        if needs_onboarding(&components) {
            crate::onboarding_ui::OnboardingUi::present(&application, components);
        } else {
            build_settings_window(&application);
        }
        drop(hold);
    });
}

fn build_settings_window(application: &adw::Application) {
    let window = ui::MainWindow::new(application);
    let general_nav = window.general_nav();
    let backend_nav = window.backend_nav();
    let diagnostics_nav = window.diagnostics_nav();
    let view_stack = window.view_stack();
    let overlay = window.overlay();

    general_nav.replace(&[status_page(
        &gettextrs::gettext("Loading Myna Settings"),
        &gettextrs::gettext("Reading the installed settings schema…"),
        "content-loading-symbolic",
    )]);
    backend_nav.replace(&[status_page(
        &gettextrs::gettext("Model"),
        &gettextrs::gettext("Model details appear once Myna finds your models."),
        "content-loading-symbolic",
    )]);
    let diagnostics_page = status_page(
        &gettextrs::gettext("About and Diagnostics"),
        &gettextrs::gettext("Diagnostics appear once Myna finds your models."),
        "dialog-information-symbolic",
    );
    diagnostics_nav.replace(std::slice::from_ref(&diagnostics_page));
    install_appearance_policy(window.upcast_ref());
    window.present();

    glib::idle_add_local_once(glib::clone!(
        #[weak]
        general_nav,
        #[weak]
        backend_nav,
        #[weak]
        diagnostics_nav,
        #[weak]
        view_stack,
        #[weak]
        overlay,
        #[strong]
        diagnostics_page,
        #[weak]
        window,
        move || {
            let (myna_page, general_settings) = match GioClientSettings::open() {
                Ok(settings) => {
                    let writer = PersistenceWriter::spawn(GioClientSettings::open);
                    let controller =
                        MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>);
                    let spoken_language = crate::backend_ui::spoken_language_group();
                    let page =
                        build_myna_page(controller.clone(), writer, &overlay, &spoken_language);
                    let general = crate::backend_ui::GeneralSettings {
                        spoken_language,
                        controller,
                    };
                    (page, Some(general))
                }
                Err(error) => (error_page(&error.to_string()), None),
            };
            general_nav.replace(std::slice::from_ref(&myna_page));

            let ui = crate::backend_ui::BackendUi::install(
                &view_stack,
                &backend_nav,
                &diagnostics_nav,
                &overlay,
                myna_page,
                general_settings,
                diagnostics_page,
            );
            ui.install_window_actions(&window);
            window.connect_close_request(move |_| {
                ui.shutdown();
                glib::Propagation::Proceed
            });
        }
    ));
}

fn current_appearance_policy() -> AppearancePolicy {
    appearance_policy(
        gtk::Settings::default()
            .map(|settings| settings.is_gtk_enable_animations())
            .unwrap_or(true),
        adw::StyleManager::default().is_high_contrast()
            || gio::Settings::new("org.gnome.desktop.a11y.interface").boolean("high-contrast"),
    )
}

fn apply_appearance_policy(window: &gtk::Widget) {
    let policy = current_appearance_policy();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
    if policy.reduced_motion {
        window.add_css_class("reduced-motion");
    } else {
        window.remove_css_class("reduced-motion");
    }
    if policy.high_contrast {
        window.add_css_class("high-contrast");
    } else {
        window.remove_css_class("high-contrast");
    }
}

pub(crate) fn install_appearance_policy(window: &gtk::Widget) {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource("/com/canonical/Myna/Config/ui/appearance.css");
    let display = gtk::prelude::WidgetExt::display(window);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    if gtk::check_version(4, 16, 0).is_none() {
        let modern = gtk::CssProvider::new();
        modern.load_from_resource("/com/canonical/Myna/Config/ui/appearance-gtk416.css");
        gtk::style_context_add_provider_for_display(
            &display,
            &modern,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    // The application id's resource path covers this only under that id, and
    // the probes run under others.
    let icons = gtk::IconTheme::for_display(&display);
    if !icons
        .resource_path()
        .iter()
        .any(|path| path == ICON_RESOURCES)
    {
        icons.add_resource_path(ICON_RESOURCES);
    }
    apply_appearance_policy(window);
    if let Some(settings) = gtk::Settings::default() {
        settings.connect_gtk_enable_animations_notify(glib::clone!(
            #[weak]
            window,
            move |_| apply_appearance_policy(&window)
        ));
    }
    let accessibility_settings = gio::Settings::new("org.gnome.desktop.a11y.interface");
    accessibility_settings.connect_changed(
        Some("high-contrast"),
        glib::clone!(
            #[weak]
            window,
            move |_, _| apply_appearance_policy(&window)
        ),
    );
    window.connect_destroy(move |_| {
        // Keep the settings source alive for the lifetime of its window.
        let _ = &accessibility_settings;
    });
    adw::StyleManager::default().connect_high_contrast_notify(glib::clone!(
        #[weak]
        window,
        move |_| apply_appearance_policy(&window)
    ));
}

/// Type into a text row and let its write land, asserting the row is still
/// focused and editable afterwards.
///
/// The regression: `apply` desensitized a row while its own write was in
/// flight. Doing that to a focused `AdwEntryRow` takes focus away mid-word and
/// makes GTK complain that its `GtkText` never received a focus-out.
fn typing_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config typing probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    // libadwaita's own init: the widgets below are built before
    // `Application::run` would have done it. No `Application` is attached -
    // the probe only needs a realized toplevel to hold keyboard focus, and
    // `gtk_window_set_application` crashes against an unstarted one.
    adw::init().expect("libadwaita init");

    let settings = match GioClientSettings::open() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("myna-config typing probe could not open the settings store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let controller = MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>);
    let writer = PersistenceWriter::spawn(GioClientSettings::open);
    let overlay = adw::ToastOverlay::new();
    let PageState::Ready(rows) = controller.state() else {
        eprintln!("myna-config typing probe found no settings rows");
        return glib::ExitCode::FAILURE;
    };
    // The only text row is the spoken language, which the Model tab shows.
    let model_settings = adw::PreferencesGroup::new();
    let _page = ready_page(controller, writer, rows, &overlay, &model_settings);
    overlay.set_child(Some(&model_settings));
    let window = adw::Window::builder().content(&overlay).build();
    window.present();
    settle_gtk();

    let Some(row) = first_entry_row(overlay.upcast_ref::<gtk::Widget>()) else {
        eprintln!("myna-config typing probe found no text row");
        return glib::ExitCode::FAILURE;
    };
    // The window's focus widget, not `has_focus()`: an unmapped probe window
    // never gets keyboard focus from the compositor, and it is the *window's*
    // focus moving that this regression is about. `grab_focus` on an
    // `AdwEntryRow` lands on the internal `GtkText`, so the test is whether
    // focus is anywhere inside the row.
    let focus_in_row = || {
        gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|widget| {
            widget == *row.upcast_ref::<gtk::Widget>() || widget.is_ancestor(&row)
        })
    };
    row.grab_focus();
    settle_gtk();
    if !focus_in_row() {
        eprintln!("typing probe could not focus the text row");
        return glib::ExitCode::FAILURE;
    }
    let original = row.text().to_string();
    row.set_text("xx");
    // Longer than the 250 ms debounce, so the write is issued and completed.
    for _ in 0..8 {
        settle_gtk();
    }
    if !focus_in_row() {
        eprintln!("focus left the text row while its write was in flight");
        return glib::ExitCode::FAILURE;
    }
    if !row.is_sensitive() {
        eprintln!("the text row was desensitized while its write was in flight");
        return glib::ExitCode::FAILURE;
    }
    println!("typing-focus: retained");
    row.set_text(&original);
    for _ in 0..8 {
        settle_gtk();
    }
    glib::ExitCode::SUCCESS
}

/// The part of the daemon's interface Myna Settings uses, served in-process by
/// the shortcut probe.
// Single-quoted attributes: xgettext cannot parse a raw string literal.
const PROBE_DICTATION_XML: &str = "<node>\
  <interface name='com.canonical.Myna.Dictation'>\
    <method name='BindShortcut'>\
      <arg name='preferred' type='s' direction='in'/>\
      <arg name='ok' type='b' direction='out'/>\
      <arg name='message' type='s' direction='out'/>\
    </method>\
    <property name='Shortcut' type='s' access='read'/>\
    <property name='Activation' type='s' access='read'/>\
  </interface>\
</node>";

/// Finishing setup under control activation installs the default key, unless
/// the user already has one or the key is taken; under the portal it raises
/// the portal's own dialog, the only way to grant one. Runs against a stand-in
/// daemon on the session bus, which the caller makes private.
fn onboarding_control_probe() -> glib::ExitCode {
    use crate::adapters::desktop_shortcut::DesktopShortcut;
    use crate::onboarding::{assess, Machine};
    use crate::onboarding_ui::OnboardingUi;

    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config onboarding control probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    let application = new_application(&probe_app_id());
    let _ = application.register(None::<&gio::Cancellable>);

    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        eprintln!("myna-config onboarding control probe needs a session bus");
        return glib::ExitCode::FAILURE;
    };
    let Some(interface) = gio::DBusNodeInfo::for_xml(PROBE_DICTATION_XML)
        .ok()
        .and_then(|node| node.lookup_interface("com.canonical.Myna.Dictation"))
    else {
        eprintln!("the probe's daemon interface did not parse");
        return glib::ExitCode::FAILURE;
    };
    let activation = Rc::new(RefCell::new(String::new()));
    let shortcut = Rc::new(RefCell::new(String::new()));
    let binds = Rc::new(Cell::new(0));
    let registered = connection
        .register_object("/com/canonical/Myna/Dictation", &interface)
        .method_call({
            let binds = binds.clone();
            move |_, _, _, _, _, _, invocation| {
                binds.set(binds.get() + 1);
                invocation.return_value(Some(&(false, "the probe binds nothing").to_variant()));
            }
        })
        .property({
            let activation = activation.clone();
            let shortcut = shortcut.clone();
            move |_, _, _, _, property| match property {
                "Activation" => activation.borrow().to_variant(),
                _ => shortcut.borrow().to_variant(),
            }
        })
        .build();
    let owned = connection.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        Some(&("com.canonical.Myna.Dictation", 4u32).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        1_000,
        gio::Cancellable::NONE,
    );
    if registered.is_err() || owned.is_err() {
        eprintln!("the probe could not serve its stand-in daemon");
        return glib::ExitCode::FAILURE;
    }
    let Some(desktop) = DesktopShortcut::open() else {
        eprintln!("the probe finds no media-keys schema");
        return glib::ExitCode::FAILURE;
    };
    let installed = [crate::diagnostics::InstalledSnap {
        name: crate::onboarding::MYNA_SNAP.to_owned(),
        version: "1".to_owned(),
    }];
    let step = |window: &ui::OnboardingWindow| {
        window
            .navigation()
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    };
    // Walk a fully installed machine through setup to the shortcut step,
    // never touching the shortcut button.
    let walk = |mode: &str| {
        activation.replace(mode.to_owned());
        let machine = ProbeMachine::new();
        let (window, button) = {
            let ui = OnboardingUi::present_with_ports(
                &application,
                assess(Machine {
                    user_daemons: true,
                    ..Machine::new(&installed, 1)
                }),
                Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                    std::sync::Arc::new(machine.clone()),
                )),
                Rc::new(machine),
                ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
                crate::onboarding_ui::Opener::FirstRun,
            );
            (ui.window(), ui.shortcut_button())
        };
        settle_gtk();
        window.forward_button().emit_clicked();
        settle_gtk();
        window.forward_button().emit_clicked();
        for _ in 0..100 {
            if step(&window) == "shortcut" {
                break;
            }
            settle_gtk();
        }
        for _ in 0..5 {
            settle_gtk();
        }
        (window, button)
    };
    let dictation = gettextrs::gettext("Dictation");
    let toggle = format!("/snap/bin/{}.toggle", crate::onboarding::MYNA_SNAP);

    let _ = desktop.install(&dictation, &toggle, "<Control><Alt>d");
    let (window, _) = walk("control");
    if desktop.binding().as_deref() != Some("<Control><Alt>d") {
        eprintln!("setup replaced the user's key with {:?}", desktop.binding());
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-default: kept the user's key");
    window.close();
    let _ = desktop.install(&dictation, &toggle, "");

    let theirs = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";
    let list = gio::Settings::new("org.gnome.settings-daemon.plugins.media-keys");
    let other = gio::Settings::with_path(
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding",
        theirs,
    );
    let mut paths: Vec<String> = list
        .strv("custom-keybindings")
        .iter()
        .map(|path| path.to_string())
        .collect();
    paths.push(theirs.to_owned());
    let _ = list.set_strv("custom-keybindings", paths);
    let _ = other.set_string("binding", "<Super>j");
    let (window, _) = walk("control");
    if desktop.binding().is_some() || other.string("binding") != "<Super>j" {
        eprintln!(
            "setup took a key another shortcut holds: {:?}",
            desktop.binding()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-default: left a key in use");
    window.close();
    let _ = other.set_string("binding", "");

    // Under the portal only its own dialog grants a key, so arriving raises
    // it once. The stand-in answers as a dismissed dialog: that was the
    // user's answer, so nothing reports it, and setting a key up becomes the
    // step's main action over Done.
    let (window, button) = walk("portal");
    for _ in 0..20 {
        if binds.get() > 0 {
            break;
        }
        settle_gtk();
    }
    settle_gtk();
    if binds.get() != 1 || desktop.binding().is_some() || window.visible_dialog().is_some() {
        eprintln!(
            "setup under the portal bound {} times, installed {:?}, dialog {}",
            binds.get(),
            desktop.binding(),
            window.visible_dialog().is_some()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-default: portal dialog raised on arrival");
    let forward = window.forward_button();
    if !standard_button(&button, true)
        || !standard_button(&forward, false)
        || !forward.is_sensitive()
    {
        eprintln!(
            "with no key the step suggested {:?} over {:?}",
            forward.css_classes(),
            button.css_classes()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: set up leads while no key is bound");
    window.close();
    binds.set(0);

    // A key the portal granted before reads as the design, as Super+J.
    shortcut.replace("Press <Super>j".to_owned());
    let (window, button) = walk("portal");
    shortcut.replace(String::new());
    if !shortcut_shown(&window) {
        eprintln!(
            "the portal's Super+J did not show as the design: caps {:?}",
            keycaps(window.upcast_ref())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: Super+J under the portal");
    let forward = window.forward_button();
    if binds.get() != 0 || !standard_button(&forward, true) || !standard_button(&button, false) {
        eprintln!(
            "with a key bound setup asked {} binds and suggested {:?} over {:?}",
            binds.get(),
            button.css_classes(),
            forward.css_classes()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: Done leads once a key is bound");
    // A rebind in the desktop's settings reaches the page as the daemon's
    // property changing.
    shortcut.replace("Press <Control><Alt>k".to_owned());
    let changed =
        std::collections::HashMap::from([("Shortcut".to_owned(), shortcut.borrow().to_variant())]);
    let _ = connection.emit_signal(
        None,
        "/com/canonical/Myna/Dictation",
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        Some(
            &(
                "com.canonical.Myna.Dictation",
                changed,
                Vec::<String>::new(),
            )
                .to_variant(),
        ),
    );
    for _ in 0..20 {
        if keycaps(window.upcast_ref()) == ["Ctrl", "Alt", "K"] {
            break;
        }
        settle_gtk();
    }
    shortcut.replace(String::new());
    if keycaps(window.upcast_ref()) != ["Ctrl", "Alt", "K"] {
        eprintln!(
            "a rebind under the portal left the caps {:?}",
            keycaps(window.upcast_ref())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: follows a portal rebind");
    window.close();

    let (window, button) = walk("control");
    for _ in 0..40 {
        if desktop.binding().is_some() {
            break;
        }
        settle_gtk();
    }
    settle_gtk();
    if step(&window) != "shortcut"
        || desktop.binding().as_deref() != Some(crate::shortcut::DEFAULT_ACCELERATOR)
        || button.label().as_deref() != Some("Change shortcut")
    {
        eprintln!(
            "setup under control left binding {:?} and offered {:?}",
            desktop.binding(),
            button.label()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-default: Super+J without a click");
    if !shortcut_shown(&window) {
        eprintln!(
            "the desktop's Super+J did not show as the design: caps {:?}",
            keycaps(window.upcast_ref())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: Super+J under control");
    // Myna Settings' own row and GNOME's keyboard settings both rewrite the
    // desktop shortcut; the page follows either.
    let _ = desktop.install(&dictation, &toggle, "<Super>k");
    for _ in 0..20 {
        if keycaps(window.upcast_ref()) == ["Super", "K"] {
            break;
        }
        settle_gtk();
    }
    if keycaps(window.upcast_ref()) != ["Super", "K"] {
        eprintln!(
            "a new desktop key left the caps {:?}",
            keycaps(window.upcast_ref())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-keys: follows a desktop rebind");
    window.close();
    let _ = desktop.install(&dictation, &toggle, "");
    settle_gtk();

    // The restarted daemon takes a moment to claim its name; moving on
    // before then showed "not running" on arrival. Setup waits for it.
    let name_call = |method: &str| {
        connection.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&match method {
                "RequestName" => ("com.canonical.Myna.Dictation", 4u32).to_variant(),
                _ => ("com.canonical.Myna.Dictation",).to_variant(),
            }),
            None,
            gio::DBusCallFlags::NONE,
            1_000,
            gio::Cancellable::NONE,
        )
    };
    shortcut.replace("Press <Super>j".to_owned());
    activation.replace("portal".to_owned());
    if name_call("ReleaseName").is_err() {
        eprintln!("the probe could not stop its stand-in daemon");
        return glib::ExitCode::FAILURE;
    }
    let window = OnboardingUi::present_with_ports(
        &application,
        assess(Machine {
            user_daemons: true,
            ..Machine::new(&installed, 1)
        }),
        Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(ProbeMachine::new()),
        )),
        Rc::new(ProbeMachine::new()),
        ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
        crate::onboarding_ui::Opener::FirstRun,
    )
    .window();
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    window.forward_button().emit_clicked();
    for _ in 0..10 {
        settle_gtk();
    }
    if step(&window) != "components" {
        eprintln!("setup moved on before the restarted daemon was on the bus");
        return glib::ExitCode::FAILURE;
    }
    if name_call("RequestName").is_err() {
        eprintln!("the probe could not restart its stand-in daemon");
        return glib::ExitCode::FAILURE;
    }
    for _ in 0..100 {
        if step(&window) == "shortcut" {
            break;
        }
        settle_gtk();
    }
    if step(&window) != "shortcut" || !shortcut_shown(&window) {
        eprintln!(
            "once the daemon was up the wizard reached {} with caps {:?}",
            step(&window),
            keycaps(window.upcast_ref())
        );
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-restart: waits for the daemon's name");
    window.close();
    settle_gtk();
    glib::ExitCode::SUCCESS
}

/// Drive the Myna page's shortcut row against a stand-in daemon on the session
/// bus, which the caller makes private. Under `control` activation the daemon
/// is never asked to bind: the row installs the desktop shortcut itself.
fn shortcut_probe(control: bool) -> glib::ExitCode {
    use std::collections::HashMap;

    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config shortcut probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    adw::init().expect("libadwaita init");

    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        eprintln!("myna-config shortcut probe needs a session bus");
        return glib::ExitCode::FAILURE;
    };
    let Some(interface) = gio::DBusNodeInfo::for_xml(PROBE_DICTATION_XML)
        .ok()
        .and_then(|node| node.lookup_interface("com.canonical.Myna.Dictation"))
    else {
        eprintln!("the probe's daemon interface did not parse");
        return glib::ExitCode::FAILURE;
    };
    let shortcut = Rc::new(RefCell::new(String::new()));
    let asked = Rc::new(RefCell::new(None::<String>));
    // The first bind is refused, the way a portal without GlobalShortcuts does.
    let refused = Rc::new(Cell::new(false));
    let registered = connection
        .register_object("/com/canonical/Myna/Dictation", &interface)
        .method_call({
            let shortcut = shortcut.clone();
            let asked = asked.clone();
            let refused = refused.clone();
            move |connection, _, path, interface, _, parameters, invocation| {
                if !refused.replace(true) {
                    invocation.return_value(Some(
                        &(false, "the portal offers no GlobalShortcuts").to_variant(),
                    ));
                    return;
                }
                asked.replace(parameters.get::<(String,)>().map(|(preferred,)| preferred));
                shortcut.replace("Press <Super>j".to_owned());
                let changed =
                    HashMap::from([("Shortcut".to_owned(), shortcut.borrow().to_variant())]);
                let _ = connection.emit_signal(
                    None,
                    path,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    Some(
                        &(interface.unwrap_or_default(), changed, Vec::<String>::new())
                            .to_variant(),
                    ),
                );
                invocation.return_value(Some(&(true, "bound to Press <Super>j").to_variant()));
            }
        })
        .property({
            let shortcut = shortcut.clone();
            move |_, _, _, _, property| match property {
                "Activation" if control => "control".to_variant(),
                "Activation" => "portal".to_variant(),
                _ => shortcut.borrow().to_variant(),
            }
        })
        .build();
    let owned = connection.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        Some(&("com.canonical.Myna.Dictation", 4u32).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        1_000,
        gio::Cancellable::NONE,
    );
    if registered.is_err() || owned.is_err() {
        eprintln!("the probe could not serve its stand-in daemon");
        return glib::ExitCode::FAILURE;
    }

    let settings = match GioClientSettings::open() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("myna-config shortcut probe could not open the settings store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let controller = MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>);
    let writer = PersistenceWriter::spawn(GioClientSettings::open);
    let overlay = adw::ToastOverlay::new();
    let PageState::Ready(rows) = controller.state() else {
        eprintln!("myna-config shortcut probe found no settings rows");
        return glib::ExitCode::FAILURE;
    };
    let page = ready_page(
        controller,
        writer,
        rows,
        &overlay,
        &adw::PreferencesGroup::new(),
    );
    let Ok(myna) = page.clone().downcast::<ui::MynaPage>() else {
        eprintln!("the settings page is not the Myna page");
        return glib::ExitCode::FAILURE;
    };
    overlay.set_child(Some(&page));
    let window = adw::Window::builder().content(&overlay).build();
    window.present();

    let settles = |done: &dyn Fn() -> bool| {
        for _ in 0..40 {
            if done() {
                return true;
            }
            settle_gtk();
        }
        done()
    };
    let button = myna.shortcut_button();
    let keys = myna.shortcut_keys();
    // The row shows the key as one line of dim text, not key caps.
    let shown = || {
        let mut text = Vec::new();
        let mut child = keys.first_child();
        while let Some(widget) = child {
            match widget.clone().downcast::<gtk::Label>() {
                Ok(label) if label.has_css_class("dim-label") && !label.has_css_class("keycap") => {
                    text.push(label.label().to_string());
                }
                _ => text.push(format!("<{}>", widget.type_().name())),
            }
            child = widget.next_sibling();
        }
        text.join("")
    };

    if myna.shortcut_group().title() != "Keyboard shortcut"
        || myna.shortcut_row().title() != "Press to start and stop"
    {
        eprintln!(
            "the shortcut row reads {:?} / {:?}",
            myna.shortcut_group().title(),
            myna.shortcut_row().title()
        );
        return glib::ExitCode::FAILURE;
    }
    if !settles(&|| button.is_sensitive()) {
        eprintln!("the shortcut row never offered to bind against a running daemon");
        return glib::ExitCode::FAILURE;
    }
    if button.label().as_deref() != Some("Set up") || keys.is_visible() {
        eprintln!(
            "an unbound daemon rendered {:?} with keys visible: {}",
            button.label(),
            keys.is_visible()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("shortcut-unbound: offered set-up");

    if !control {
        button.emit_clicked();
        if !settles(&|| window.visible_dialog().is_some()) {
            eprintln!("a refused bind showed no error dialog");
            return glib::ExitCode::FAILURE;
        }
        if let Some(dialog) = window.visible_dialog() {
            dialog.force_close();
        }
        println!("shortcut-refused: error dialog");
    }

    button.emit_clicked();
    if !settles(&|| !shown().is_empty()) {
        eprintln!("the granted shortcut never rendered");
        return glib::ExitCode::FAILURE;
    }
    if shown() != "Super + J" {
        eprintln!("expected Super + J as dim text, got {:?}", shown());
        return glib::ExitCode::FAILURE;
    }
    let expected_ask = if control { None } else { Some("") };
    if asked.borrow().as_deref() != expected_ask {
        eprintln!(
            "set-up asked the daemon for {:?}, expected {expected_ask:?}",
            asked.borrow()
        );
        return glib::ExitCode::FAILURE;
    }
    if button.label().as_deref() != Some("Change") {
        eprintln!("a bound shortcut offered {:?}", button.label());
        return glib::ExitCode::FAILURE;
    }
    println!("shortcut-bound: Super+J");

    if control {
        button.emit_clicked();
        settles(&|| window.visible_dialog().is_some());
        let Some(dialog) = window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<ui::ShortcutDialog>().ok())
        else {
            eprintln!("Change opened no capture dialog");
            return glib::ExitCode::FAILURE;
        };
        // Key events travel only to the focused widget and its ancestors.
        if !settles(&|| {
            gtk::prelude::GtkWindowExt::focus(&window)
                .is_some_and(|focus| focus.is_ancestor(&dialog))
        }) {
            eprintln!("the capture dialog does not hold keyboard focus");
            return glib::ExitCode::FAILURE;
        }
        // The example is the default key, drawn as the onboarding step draws it.
        if keycaps(dialog.upcast_ref()) != ["Super", "J"] {
            eprintln!(
                "the capture dialog's example reads {:?}",
                keycaps(dialog.upcast_ref())
            );
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-dialog: example is the default key");
        if !keycaps_dimmed(dialog.upcast_ref()) {
            eprintln!("the capture dialog's example caps read as a captured key");
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-dialog: example dimmed");
        dialog.press(
            gtk::gdk::Key::d,
            gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK,
        );
        if !settles(&|| shown() == "Ctrl + Alt + D") {
            eprintln!("the captured shortcut rendered as {:?}", shown());
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-changed: Ctrl+Alt+D");

        // A bare letter would take over typing; a key with no text, such as
        // the Calculator key, cannot.
        button.emit_clicked();
        settles(&|| window.visible_dialog().is_some());
        let Some(dialog) = window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<ui::ShortcutDialog>().ok())
        else {
            eprintln!("Change opened no capture dialog the second time");
            return glib::ExitCode::FAILURE;
        };
        dialog.press(gtk::gdk::Key::a, gtk::gdk::ModifierType::empty());
        if window.visible_dialog().is_none() {
            eprintln!("a bare letter was captured as the shortcut");
            return glib::ExitCode::FAILURE;
        }
        dialog.press(gtk::gdk::Key::Calculator, gtk::gdk::ModifierType::empty());
        let binding = crate::adapters::desktop_shortcut::DesktopShortcut::open()
            .and_then(|desktop| desktop.binding());
        if binding.as_deref() != Some("XF86Calculator") {
            eprintln!("the Calculator key was stored as {binding:?}");
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-special-key: Calculator");

        // Super+L locks the screen: taking it asks first, then moves it.
        button.emit_clicked();
        settles(&|| window.visible_dialog().is_some());
        let Some(dialog) = window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<ui::ShortcutDialog>().ok())
        else {
            eprintln!("Change opened no capture dialog the third time");
            return glib::ExitCode::FAILURE;
        };
        // Super+O is rotation lock's -static key, which cannot be taken.
        dialog.press(gtk::gdk::Key::o, gtk::gdk::ModifierType::SUPER_MASK);
        let refusal = dialog.refusal();
        if window.visible_dialog().as_ref() != Some(dialog.upcast_ref())
            || !refusal
                .as_deref()
                .is_some_and(|text| text.contains("Toggle automatic screen orientation"))
        {
            eprintln!("a reserved key was not refused in the dialog: {refusal:?}");
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-reserved: Super+O refused");
        dialog.press(gtk::gdk::Key::l, gtk::gdk::ModifierType::SUPER_MASK);
        settles(&|| {
            window
                .visible_dialog()
                .is_some_and(|dialog| dialog.is::<adw::AlertDialog>())
        });
        let Some(alert) = window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<adw::AlertDialog>().ok())
        else {
            eprintln!("taking Lock screen's key asked nothing");
            return glib::ExitCode::FAILURE;
        };
        alert.emit_by_name::<()>("response", &[&"replace"]);
        let desktop = crate::adapters::desktop_shortcut::DesktopShortcut::open();
        let binding = desktop.as_ref().and_then(|desktop| desktop.binding());
        let still_held = desktop
            .as_ref()
            .and_then(|desktop| desktop.conflict("<Super>l"));
        if binding.as_deref() != Some("<Super>l") || still_held.is_some() {
            eprintln!("replacing left binding {binding:?}, conflict {still_held:?}");
            return glib::ExitCode::FAILURE;
        }
        println!("shortcut-replaced: Super+L");
    }
    glib::ExitCode::SUCCESS
}

/// A machine with Parakeet connected and Whisper installed, answering from the
/// fixtures the repository adapter's own tests use. It is also the privileged
/// configurator: a `set` it executes is what the next `get` returns, so an
/// apply reads back the way it does on a real backend.
#[derive(Clone)]
struct ProbeMachine {
    configuration: std::sync::Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    applied: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Nothing is installed yet: every `snap` read fails, as on a bare machine.
    bare: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// snapd's flag is on however bare the machine.
    flagged: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// A daemon restart, alone or ending a switch, waits while this is set,
    /// as a slow one does.
    held: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// How many daemon restarts to refuse before letting one through.
    refusals: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// snapd is still installing the model while this is set.
    installing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    change_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    restarts_attempted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The backends `myna:backend` is connected to; switches rewire it.
    connected: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// The user dismisses the authorization prompt of every switch.
    dismissing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// snapd refuses every switch.
    denying: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// polkit refuses every privileged apply.
    refusing_applies: std::sync::Arc<std::sync::atomic::AtomicBool>,
    switches_attempted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// How snapd answers the next writes of the flag; success once empty.
    flag_answers: std::sync::Arc<
        std::sync::Mutex<
            std::collections::VecDeque<Result<(), crate::ports::SystemConfiguratorError>>,
        >,
    >,
    /// A flag write waits while this is set, as while polkit's prompt is open.
    holding_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    flag_writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// A flag read answers what it read only once this is cleared.
    holding_reads: std::sync::Arc<std::sync::atomic::AtomicBool>,
    flag_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Myna is installed and no backend yet.
    myna_only: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The snaps asked for, in order.
    installs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// How snapd answers the next install requests; `change-<snap>` once
    /// empty.
    install_answers: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<InstallAnswer>>>,
    /// An install request waits while this is set, as while polkit asks.
    holding_install: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// While set, change reads report this percentage of the download
    /// fetched.
    downloading: std::sync::Arc<std::sync::Mutex<Option<u64>>>,
    /// How the change ends: snapd's error, or done when none.
    install_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Install changes started outside the wizard, as `select=in-progress`
    /// lists them.
    pending_installs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl ProbeMachine {
    fn new() -> Self {
        let configuration = include_str!("../tests/fixtures/modelctl-get.txt")
            .lines()
            .filter_map(|line| line.split_once(": "))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        Self {
            configuration: std::sync::Arc::new(std::sync::Mutex::new(configuration)),
            applied: std::sync::Arc::default(),
            reads: std::sync::Arc::default(),
            bare: std::sync::Arc::default(),
            flagged: std::sync::Arc::default(),
            held: std::sync::Arc::default(),
            refusals: std::sync::Arc::default(),
            installing: std::sync::Arc::default(),
            change_reads: std::sync::Arc::default(),
            restarts_attempted: std::sync::Arc::default(),
            connected: std::sync::Arc::new(std::sync::Mutex::new(vec!["myna-parakeet".to_owned()])),
            dismissing: std::sync::Arc::default(),
            denying: std::sync::Arc::default(),
            refusing_applies: std::sync::Arc::default(),
            switches_attempted: std::sync::Arc::default(),
            flag_answers: std::sync::Arc::default(),
            holding_flag: std::sync::Arc::default(),
            flag_writes: std::sync::Arc::default(),
            holding_reads: std::sync::Arc::default(),
            flag_reads: std::sync::Arc::default(),
            myna_only: std::sync::Arc::default(),
            installs: std::sync::Arc::default(),
            install_answers: std::sync::Arc::default(),
            holding_install: std::sync::Arc::default(),
            downloading: std::sync::Arc::default(),
            install_error: std::sync::Arc::default(),
            pending_installs: std::sync::Arc::default(),
        }
    }

    fn answer_install(&self, answer: InstallAnswer) {
        self.install_answers
            .lock()
            .expect("probe machine lock")
            .push_back(answer);
    }

    fn hold_install(&self, holding: bool) {
        self.holding_install
            .store(holding, std::sync::atomic::Ordering::SeqCst);
    }

    /// Change reads report `done` percent fetched until this is `None`.
    fn download(&self, done: Option<u64>) {
        *self.downloading.lock().expect("probe machine lock") = done;
    }

    fn fail_installs(&self, error: Option<&str>) {
        *self.install_error.lock().expect("probe machine lock") = error.map(str::to_owned);
    }

    fn installs(&self) -> Vec<String> {
        self.installs.lock().expect("probe machine lock").clone()
    }

    /// A change installing `snap` is running, started elsewhere.
    fn installing_elsewhere(&self, snap: &str) {
        self.pending_installs
            .lock()
            .expect("probe machine lock")
            .push(snap.to_owned());
    }

    /// What installing `snap` leaves on the machine.
    fn installed(&self, snap: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        if snap == crate::onboarding::MYNA_SNAP && self.bare.load(SeqCst) {
            self.myna_only.store(true, SeqCst);
            self.bare.store(false, SeqCst);
        } else if snap != crate::onboarding::MYNA_SNAP {
            self.myna_only.store(false, SeqCst);
            self.bare.store(false, SeqCst);
        }
    }

    fn answer_flag(&self, answer: Result<(), crate::ports::SystemConfiguratorError>) {
        self.flag_answers
            .lock()
            .expect("probe machine lock")
            .push_back(answer);
    }

    fn hold_flag(&self, holding: bool) {
        self.holding_flag
            .store(holding, std::sync::atomic::Ordering::SeqCst);
    }

    fn hold_reads(&self, holding: bool) {
        self.holding_reads
            .store(holding, std::sync::atomic::Ordering::SeqCst);
    }

    fn flag_reads(&self) -> usize {
        self.flag_reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn flag_writes(&self) -> usize {
        self.flag_writes.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn dismiss_authorization(&self, dismissing: bool) {
        self.dismissing
            .store(dismissing, std::sync::atomic::Ordering::SeqCst);
    }

    fn refuse_applies(&self, refusing: bool) {
        self.refusing_applies
            .store(refusing, std::sync::atomic::Ordering::SeqCst);
    }

    fn deny_switches(&self, denying: bool) {
        self.denying
            .store(denying, std::sync::atomic::Ordering::SeqCst);
    }

    /// No backend is connected to `myna:backend`.
    fn disconnect_backends(&self) {
        self.connected.lock().expect("probe machine lock").clear();
    }

    fn switches_attempted(&self) -> usize {
        self.switches_attempted
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn connections(&self) -> String {
        let connected = self.connected.lock().expect("probe machine lock");
        let mut table = "Interface  Plug  Slot  Notes\n".to_owned();
        for snap in ["myna-parakeet", "myna-whisper"] {
            table.push_str(&if connected.iter().any(|backend| backend == snap) {
                format!("content[inference-provider]  myna:backend  {snap}:provider  manual\n")
            } else {
                format!("content  -  {snap}:provider  -\n")
            });
        }
        table + "network  browser:network  :network  -\n"
    }

    fn hold_changes(&self, installing: bool) {
        self.installing
            .store(installing, std::sync::atomic::Ordering::SeqCst);
    }

    fn change_reads(&self) -> usize {
        self.change_reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn restarts_attempted(&self) -> usize {
        self.restarts_attempted
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn refuse_restarts(&self, count: usize) {
        self.refusals
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    fn hold_restart(&self, held: bool) {
        self.held.store(held, std::sync::atomic::Ordering::SeqCst);
    }

    /// snapd's flag is on and nothing is installed yet.
    fn flagged() -> Self {
        let machine = Self::bare();
        machine
            .flagged
            .store(true, std::sync::atomic::Ordering::SeqCst);
        machine
    }

    /// The machine before anything is installed, the flag off.
    fn bare() -> Self {
        let machine = Self::new();
        machine
            .bare
            .store(true, std::sync::atomic::Ordering::SeqCst);
        machine
    }

    fn install(&self) {
        self.bare.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn applied(&self) -> Vec<Vec<String>> {
        self.applied.lock().expect("probe machine lock").clone()
    }

    fn snap(&self, arguments: &[&str]) -> Option<String> {
        if self.bare.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let fixture = |text: &str| Some(text.to_owned());
        if self.myna_only.load(std::sync::atomic::Ordering::SeqCst) {
            let content = include_str!("../tests/fixtures/snap-interface-content.txt");
            return match arguments {
                ["list", "--unicode=never"] => fixture(
                    "Name  Version  Rev  Tracking  Publisher  Notes\n\
                     myna  1.2.3  7  latest/edge  canonical**  -\n",
                ),
                ["connections", "--all"] => fixture("Interface  Plug  Slot  Notes\n"),
                ["interface", "content", "--attrs"] => content
                    .split_once("  - myna-parakeet:provider:")
                    .map(|(head, _)| head.to_owned()),
                _ => None,
            };
        }
        match arguments {
            ["list", "--unicode=never"] => fixture(
                "Name  Version  Rev  Tracking  Publisher  Notes\n\
                 myna  1.2.3  7  latest/stable  canonical**  -\n\
                 myna-parakeet  0.1.0  8  latest/stable  canonical**  -\n\
                 myna-whisper  0.1.0  9  latest/stable  canonical**  -\n",
            ),
            ["connections", "--all"] => Some(self.connections()),
            ["interface", "content", "--attrs"] => {
                fixture(include_str!("../tests/fixtures/snap-interface-content.txt"))
            }
            ["info", snap] => Some(
                include_str!("../tests/fixtures/snap-info-parakeet.txt")
                    .replace("myna-parakeet", snap),
            ),
            ["run", _, "version", "--format=json"] => {
                fixture(include_str!("../tests/fixtures/modelctl-version.json"))
            }
            ["run", _, "status", "--format=json"] => {
                fixture(include_str!("../tests/fixtures/modelctl-status.json"))
            }
            ["run", _, "list-models", "--format=json"] => {
                fixture(include_str!("../tests/fixtures/modelctl-list-models.json"))
            }
            ["run", _, "list-engines", "--format=json"] => {
                fixture(include_str!("../tests/fixtures/modelctl-list-engines.json"))
            }
            ["run", _, "get"] => Some(
                self.configuration
                    .lock()
                    .expect("probe machine lock")
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}\n"))
                    .collect(),
            ),
            _ => None,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl crate::command::CommandRunner for ProbeMachine {
    async fn run(
        &self,
        request: crate::command::CommandRequest,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<crate::command::CommandOutput, crate::command::CommandError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let arguments: Vec<&str> = request.arguments().iter().map(String::as_str).collect();
        match (request.executable(), self.snap(&arguments)) {
            ("snap", Some(stdout)) => Ok(crate::command::CommandOutput::new(Some(0), stdout, "")),
            _ => Err(crate::command::CommandError::NonZero {
                exit_status: Some(1),
                stdout: String::new(),
                stderr: format!("the probe machine cannot run {request:?}"),
            }),
        }
    }
}

type InstallAnswer = Result<Option<String>, crate::ports::SystemConfiguratorError>;

/// One percent of a download bigger than any the wizard expects, so the
/// row shows the percentage the probe sets.
const PROBE_PERCENT: u64 = 1 << 32;

/// gnome-shell as the probes need it, changed as they go.
struct ProbeExtensions {
    state: Cell<crate::onboarding::ExtensionState>,
    /// Why the next enables fail; they succeed once cleared.
    failing: RefCell<Option<String>>,
    /// An enable waits while this is set, as while gnome-shell starts it.
    holding: Cell<bool>,
    enables: Cell<usize>,
}

impl ProbeExtensions {
    fn new(state: crate::onboarding::ExtensionState) -> Rc<Self> {
        Rc::new(Self {
            state: Cell::new(state),
            failing: RefCell::default(),
            holding: Cell::new(false),
            enables: Cell::new(0),
        })
    }
}

#[async_trait::async_trait(?Send)]
impl crate::ports::ShellExtensions for ProbeExtensions {
    async fn extension_state(&self, _uuid: &str) -> crate::onboarding::ExtensionState {
        self.state.get()
    }

    async fn enable_extension(
        &self,
        uuid: &str,
    ) -> Result<(), crate::ports::SystemConfiguratorError> {
        self.enables.set(self.enables.get() + 1);
        while self.holding.get() {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        if let Some(message) = self.failing.borrow().clone() {
            return Err(crate::ports::SystemConfiguratorError::dbus_execution(
                format!("org.gnome.Shell.Extensions.EnableExtension({uuid:?})"),
                message,
            ));
        }
        self.state.set(crate::onboarding::ExtensionState::Enabled);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl crate::ports::SystemConfigurator for ProbeMachine {
    async fn user_daemons_enabled(
        &self,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<bool, String> {
        self.flag_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let enabled = !self.bare.load(std::sync::atomic::Ordering::SeqCst)
            || self.flagged.load(std::sync::atomic::Ordering::SeqCst);
        while self.holding_reads.load(std::sync::atomic::Ordering::SeqCst) {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        Ok(enabled)
    }

    async fn enable_user_daemons(
        &self,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<(), crate::ports::SystemConfiguratorError> {
        self.flag_writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        while self.holding_flag.load(std::sync::atomic::Ordering::SeqCst) {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let answer = self
            .flag_answers
            .lock()
            .expect("probe machine lock")
            .pop_front()
            .unwrap_or(Ok(()));
        if answer.is_ok() {
            self.flagged
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        answer
    }

    async fn install_snap(
        &self,
        snap: &str,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<Option<String>, crate::ports::SystemConfiguratorError> {
        self.installs
            .lock()
            .expect("probe machine lock")
            .push(snap.to_owned());
        while self
            .holding_install
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let answer = self
            .install_answers
            .lock()
            .expect("probe machine lock")
            .pop_front()
            .unwrap_or_else(|| Ok(Some(format!("change-{snap}"))));
        if matches!(answer, Ok(None)) {
            self.installed(snap);
        }
        answer
    }

    async fn snap_change(
        &self,
        change_id: &str,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<crate::snap_changes::ChangeInProgress, String> {
        let snap = change_id.trim_start_matches("change-");
        let downloading = *self.downloading.lock().expect("probe machine lock");
        let error = self
            .install_error
            .lock()
            .expect("probe machine lock")
            .clone();
        let change = match (downloading, error) {
            (Some(done), _) => serde_json::json!({"id": change_id, "kind": "install-snap",
                "ready": false, "status": "Doing", "summary": format!("Install \"{snap}\" snap"),
                "tasks": [{"kind": "download-snap", "status": "Doing",
                    "progress": {"label": snap, "done": done * PROBE_PERCENT, "total": 100 * PROBE_PERCENT}}]}),
            (None, Some(err)) => serde_json::json!({"id": change_id, "kind": "install-snap",
                "ready": true, "status": "Error", "err": err,
                "summary": format!("Install \"{snap}\" snap")}),
            (None, None) => {
                self.installed(snap);
                self.pending_installs
                    .lock()
                    .expect("probe machine lock")
                    .retain(|pending| pending != snap);
                serde_json::json!({"id": change_id, "kind": "install-snap",
                    "ready": true, "status": "Done", "summary": format!("Install \"{snap}\" snap")})
            }
        };
        crate::snap_changes::parse_change(change)
    }

    async fn changes_in_progress(
        &self,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<Vec<crate::snap_changes::ChangeInProgress>, String> {
        let pending: Vec<serde_json::Value> = self
            .pending_installs
            .lock()
            .expect("probe machine lock")
            .iter()
            .map(|snap| {
                serde_json::json!({"id": format!("change-{snap}"), "kind": "install-snap",
                    "ready": false, "status": "Doing",
                    "summary": format!("Install \"{snap}\" snap from \"latest/edge\" channel")})
            })
            .collect();
        if !pending.is_empty() {
            return crate::snap_changes::parse_changes(serde_json::Value::Array(pending));
        }
        if self.bare.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the probe machine has no snapd".to_owned());
        }
        self.change_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let installing = self.installing.load(std::sync::atomic::Ordering::SeqCst);
        crate::snap_changes::parse_changes(if installing {
            serde_json::from_str(include_str!(
                "../tests/fixtures/snapd-changes-installing.json"
            ))
            .map_err(|error| error.to_string())?
        } else {
            serde_json::json!([])
        })
    }

    async fn restart_myna(
        &self,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<(), crate::ports::SystemConfiguratorError> {
        self.restarts_attempted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        while self.held.load(std::sync::atomic::Ordering::SeqCst) {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let refused = self.refusals.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |left| left.checked_sub(1),
        );
        if refused.is_ok() {
            return Err(crate::ports::SystemConfiguratorError::execution(
                "systemctl",
                ["--user", "restart", "snap.myna.myna.service"]
                    .map(str::to_owned)
                    .to_vec(),
                Some(1),
                PROBE_RESTART_FAILURE,
                PROBE_RESTART_FAILURE,
            ));
        }
        self.applied
            .lock()
            .expect("probe machine lock")
            .push(vec!["restart-myna".to_owned()]);
        Ok(())
    }

    async fn execute_backend_switch(
        &self,
        plan: &crate::active_backend::SwitchPlan,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure> {
        self.switches_attempted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        while self.held.load(std::sync::atomic::Ordering::SeqCst) {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        if self.dismissing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::ports::SystemConfiguratorFailure::new(
                Vec::new(),
                crate::ports::SystemConfiguratorError::Cancelled,
            ));
        }
        if self.denying.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::ports::SystemConfiguratorFailure::new(
                Vec::new(),
                crate::ports::SystemConfiguratorError::authorization_denied(
                    "snap",
                    vec!["disconnect".to_owned()],
                    None,
                    "access denied",
                ),
            ));
        }
        Ok(self.record(plan.operations()))
    }

    async fn apply_backend_config(
        &self,
        preview: &crate::backend_apply::ApplyPreview,
        _cancellation: crate::command::CancellationToken,
    ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure> {
        if self
            .refusing_applies
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(crate::ports::SystemConfiguratorFailure::new(
                Vec::new(),
                crate::ports::SystemConfiguratorError::authorization_denied(
                    "pkexec",
                    Vec::new(),
                    Some(127),
                    "Not authorized",
                ),
            ));
        }
        Ok(self.record(preview.operations()))
    }
}

impl ProbeMachine {
    fn record(
        &self,
        operations: &[crate::command::CommandRequest],
    ) -> Vec<crate::domain::CommandResult> {
        let mut results = Vec::new();
        for operation in operations {
            let arguments = operation.arguments().to_vec();
            if arguments.get(2).map(String::as_str) == Some("set") {
                let mut configuration = self.configuration.lock().expect("probe machine lock");
                for assignment in &arguments[3..] {
                    if let Some((key, value)) = assignment.split_once('=') {
                        configuration.insert(key.to_owned(), value.to_owned());
                    }
                }
            }
            if let [action, plug, slot] = arguments.as_slice() {
                let snap = slot.trim_end_matches(":provider").to_owned();
                let mut connected = self.connected.lock().expect("probe machine lock");
                match (action.as_str(), plug.as_str()) {
                    ("connect", "myna:backend") => connected.push(snap),
                    ("disconnect", "myna:backend") => connected.retain(|held| *held != snap),
                    _ => {}
                }
            }
            self.applied
                .lock()
                .expect("probe machine lock")
                .push(arguments.clone());
            results.push(crate::domain::CommandResult::new(
                operation.executable(),
                arguments,
                Some(0),
                "",
                "",
            ));
        }
        results
    }
}

/// Drive the backend pages through the real repository adapter against a
/// fixture machine: discovery lists the backends, the active one's page reads its
/// snapshot, and a change applies on its own and is read back, or is refused.
fn backends_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config backends probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    let application = new_application(&probe_app_id());
    let _ = application.register(None::<&gio::Cancellable>);

    let window = ui::MainWindow::new(&application);
    let view_stack = window.view_stack();
    let general_nav = window.general_nav();
    let backend_nav = window.backend_nav();
    let diagnostics_nav = window.diagnostics_nav();
    let overlay = window.overlay();
    let spoken_language = crate::backend_ui::spoken_language_group();
    let client_settings = match GioClientSettings::open() {
        Ok(settings) => MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>),
        Err(error) => {
            eprintln!("myna-config backends probe could not open the settings store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let myna_page = build_myna_page(
        client_settings.clone(),
        PersistenceWriter::spawn(GioClientSettings::open),
        &overlay,
        &spoken_language,
    );
    general_nav.replace(std::slice::from_ref(&myna_page));
    window.present();
    let general = myna_page.clone().upcast::<gtk::Widget>();

    let machine = ProbeMachine::new();
    let ui = crate::backend_ui::BackendUi::install_with_ports(
        Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(machine.clone()),
        )),
        Rc::new(machine.clone()),
        &view_stack,
        &backend_nav,
        &diagnostics_nav,
        &overlay,
        myna_page,
        Some(crate::backend_ui::GeneralSettings {
            spoken_language,
            controller: client_settings,
        }),
        status_page("About and Diagnostics", "", "dialog-information-symbolic"),
    );
    ui.install_window_actions(&window);

    let settles = |done: &dyn Fn() -> bool| {
        for _ in 0..100 {
            if done() {
                return true;
            }
            settle_gtk();
        }
        done()
    };
    let content = || {
        backend_nav
            .visible_page()
            .map(|page| page.upcast::<gtk::Widget>())
    };

    if !settles(&|| ui.controller().pages().len() == 2) {
        eprintln!(
            "discovery never listed the fixture backends ({} backends)",
            ui.controller().pages().len()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("backends-discovered: 2");

    let groups = descendants(&general, &|widget| widget.is::<adw::PreferencesGroup>())
        .into_iter()
        .filter_map(|group| group.downcast::<adw::PreferencesGroup>().ok())
        .filter(|group| group.is_visible())
        .collect::<Vec<_>>();
    let group_titles: Vec<_> = groups
        .iter()
        .map(|group| group.title().to_string())
        .collect();
    if group_titles != ["Keyboard shortcut", "Dictation", "Model"] {
        eprintln!("General's groups read {group_titles:?}");
        return glib::ExitCode::FAILURE;
    }
    let dictation_rows: Vec<_> = descendants(groups[1].upcast_ref(), &|widget| {
        widget.is::<adw::PreferencesRow>()
    })
    .into_iter()
    .filter_map(|row| row.downcast::<adw::PreferencesRow>().ok())
    .map(|row| row.title().to_string())
    .collect();
    if dictation_rows
        != [
            "Sounds on start, stop and error",
            "When to transcribe",
            "Indicator style",
            "Stop after silence (seconds)",
        ]
    {
        eprintln!("the Dictation rows read {dictation_rows:?}");
        return glib::ExitCode::FAILURE;
    }
    println!("general-order: ok");

    // General lists the installed models last, each a radio row that
    // describes its family, with the connected one chosen.
    let model_rows = || {
        let groups = descendants(&general, &|widget| widget.is::<adw::PreferencesGroup>());
        let Some(group) = groups
            .last()
            .and_then(|group| group.downcast_ref::<adw::PreferencesGroup>())
            .filter(|group| group.title() == gettextrs::gettext("Model").as_str())
            .cloned()
        else {
            return Vec::new();
        };
        descendants(group.upcast_ref(), &|widget| widget.is::<adw::ActionRow>())
            .into_iter()
            .filter_map(|row| {
                let row = row.downcast::<adw::ActionRow>().ok()?;
                let radio =
                    find_descendant(row.upcast_ref(), &|widget| widget.is::<gtk::CheckButton>())?
                        .downcast::<gtk::CheckButton>()
                        .ok()
                        .filter(|radio| radio.accessible_role() == gtk::AccessibleRole::Radio)?;
                Some((row, radio))
            })
            .collect::<Vec<_>>()
    };
    let chosen = || {
        model_rows()
            .iter()
            .filter(|(_, radio)| radio.is_active())
            .map(|(row, _)| row.title().to_string())
            .collect::<Vec<_>>()
    };
    let described = |rows: &[(adw::ActionRow, gtk::CheckButton)]| {
        rows.iter()
            .map(|(row, _)| {
                (
                    row.title().to_string(),
                    row.subtitle().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![
        (
            "Parakeet".to_owned(),
            gettextrs::gettext("Fastest and most accurate in its 25 languages"),
        ),
        (
            "Whisper".to_owned(),
            gettextrs::gettext("Most languages, with uneven accuracy"),
        ),
    ];
    if !settles(&|| described(&model_rows()) == expected && chosen() == ["Parakeet"]) {
        eprintln!(
            "the General tab's last group does not list the models as radio rows with Parakeet chosen: {:?}, chosen {:?}",
            described(&model_rows()),
            chosen()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("model-group: lists the installed models");

    // Reads already in flight finish on their own; only a read after they
    // settle is one the step under test started.
    let quiesce = || {
        let mut reads = machine.reads();
        let mut quiet = 0;
        while quiet < 5 {
            settle_gtk();
            let now = machine.reads();
            quiet = if now == reads { quiet + 1 } else { 0 };
            reads = now;
        }
        reads
    };

    // Choosing another model switches at once. Dismissing snapd's
    // authorization prompt puts the radio back without a word.
    let whisper_row = || {
        model_rows()
            .into_iter()
            .find(|(row, _)| row.title() == "Whisper")
            .map(|(row, _)| row)
            .expect("whisper row")
    };
    let toasts = || {
        descendants(overlay.upcast_ref(), &|widget| {
            widget.type_().name() == "AdwToastWidget"
        })
        .len()
    };
    let asked = std::rc::Rc::new(std::cell::Cell::new(false));
    let settles_unasked = |done: &dyn Fn() -> bool| {
        settles(&|| {
            if window.visible_dialog().is_some() {
                asked.set(true);
            }
            done()
        })
    };
    let idle = || ui.operation_coordinator().active().is_none();
    machine.dismiss_authorization(true);
    adw::prelude::ActionRowExt::activate(&whisper_row());
    if !settles_unasked(&|| machine.switches_attempted() == 1 && idle() && chosen() == ["Parakeet"])
        || asked.get()
    {
        eprintln!(
            "dismissing the prompt left {:?} chosen after {} switches, asked first: {}",
            chosen(),
            machine.switches_attempted(),
            asked.get()
        );
        return glib::ExitCode::FAILURE;
    }
    if toasts() != 0 || window.visible_dialog().is_some() {
        eprintln!("dismissing the prompt was reported");
        return glib::ExitCode::FAILURE;
    }
    println!("model-group: a dismissed prompt reverts silently");

    // While the switch runs, the target stays marked, the rows are
    // insensitive and a spinner turns at the end of the target row only.
    let spinning = || {
        model_rows()
            .iter()
            .filter(|(row, _)| {
                find_descendant(row.upcast_ref(), &|widget| {
                    widget
                        .downcast_ref::<gtk::Spinner>()
                        .is_some_and(|spinner| spinner.is_visible() && spinner.is_spinning())
                })
                .is_some()
            })
            .map(|(row, _)| row.title().to_string())
            .collect::<Vec<_>>()
    };
    let insensitive = || model_rows().iter().all(|(row, _)| !row.is_sensitive());
    machine.dismiss_authorization(false);
    machine.hold_restart(true);
    adw::prelude::ActionRowExt::activate(&whisper_row());
    if !settles_unasked(&|| {
        machine.switches_attempted() == 2
            && chosen() == ["Whisper"]
            && insensitive()
            && spinning() == ["Whisper"]
    }) || asked.get()
    {
        eprintln!(
            "a pending switch shows {:?} chosen, spinners on {:?}, rows insensitive: {}",
            chosen(),
            spinning(),
            insensitive()
        );
        machine.hold_restart(false);
        return glib::ExitCode::FAILURE;
    }
    println!("model-group: a pending switch spins on its target");
    machine.hold_restart(false);
    let connected_whisper = || {
        machine
            .applied()
            .iter()
            .any(|operation| operation == &["connect", "myna:backend", "myna-whisper:provider"])
    };
    // With no choice stored, "When to transcribe" shows what the active
    // backend does: Parakeet streams, Whisper waits.
    let mode_row = || {
        descendants(&general, &|widget| {
            widget
                .downcast_ref::<adw::ComboRow>()
                .is_some_and(|row| row.title() == "When to transcribe")
        })
        .into_iter()
        .next()
        .and_then(|row| row.downcast::<adw::ComboRow>().ok())
        .expect("When to transcribe row")
    };
    let mode_shown = || {
        mode_row()
            .selected_item()
            .and_downcast::<gtk::StringObject>()
            .map(|item| item.string().to_string())
            .unwrap_or_default()
    };
    let as_you_speak = gettextrs::gettext("As you speak");
    let when_you_stop = gettextrs::gettext("When you stop");
    if !settles_unasked(&|| connected_whisper() && idle() && chosen() == ["Whisper"]) || asked.get()
    {
        eprintln!(
            "choosing Whisper left {:?} chosen; the machine ran {:?}; asked first: {}",
            chosen(),
            machine.applied(),
            asked.get()
        );
        return glib::ExitCode::FAILURE;
    }
    if mode_shown() != when_you_stop {
        eprintln!("with Whisper active the mode shows {:?}", mode_shown());
        return glib::ExitCode::FAILURE;
    }
    if !spinning().is_empty() || insensitive() {
        eprintln!(
            "a finished switch left spinners on {:?}, rows insensitive: {}",
            spinning(),
            insensitive()
        );
        return glib::ExitCode::FAILURE;
    }
    let parakeet_row = model_rows()
        .into_iter()
        .find(|(row, _)| row.title() == "Parakeet")
        .map(|(row, _)| row)
        .expect("parakeet row");
    adw::prelude::ActionRowExt::activate(&parakeet_row);
    let connected_parakeet = || {
        machine
            .applied()
            .iter()
            .any(|operation| operation == &["connect", "myna:backend", "myna-parakeet:provider"])
    };
    if !settles_unasked(&|| connected_parakeet() && idle() && chosen() == ["Parakeet"])
        || asked.get()
    {
        eprintln!("switching back left {:?} chosen", chosen());
        return glib::ExitCode::FAILURE;
    }
    if toasts() != 0 {
        eprintln!("a successful switch was announced");
        return glib::ExitCode::FAILURE;
    }
    quiesce();
    println!("model-group: choosing a model switches to it");
    if mode_shown() != as_you_speak {
        eprintln!("with Parakeet active the mode shows {:?}", mode_shown());
        return glib::ExitCode::FAILURE;
    }
    println!("mode: shows the active backend's default");

    // Picking a mode stores it as the user's own.
    let stored_choice = || {
        GioClientSettings::open()
            .ok()
            .filter(|settings| {
                settings
                    .has_user_value(myna_core::settings::KEY_STREAMING_MODE)
                    .unwrap_or(false)
            })
            .and_then(|settings| settings.get(myna_core::settings::KEY_STREAMING_MODE).ok())
    };
    if stored_choice().is_some() {
        eprintln!("showing the default stored {:?}", stored_choice());
        return glib::ExitCode::FAILURE;
    }
    let row = mode_row();
    let batch_index = (0..row.model().map_or(0, |model| model.n_items()))
        .find(|index| {
            row.model()
                .and_then(|model| model.item(*index))
                .and_downcast::<gtk::StringObject>()
                .is_some_and(|item| item.string() == when_you_stop.as_str())
        })
        .expect("a When you stop item");
    row.set_selected(batch_index);
    if !settles(&|| stored_choice() == Some(ClientSettingValue::Choice("batch".into()))) {
        eprintln!("picking When you stop stored {:?}", stored_choice());
        return glib::ExitCode::FAILURE;
    }
    if mode_shown() != when_you_stop {
        eprintln!("after the choice the mode shows {:?}", mode_shown());
        return glib::ExitCode::FAILURE;
    }
    println!("mode: a choice is stored as the user's");

    let sounds_row = descendants(general.upcast_ref(), &|widget| {
        widget.is::<adw::SwitchRow>()
    })
    .into_iter()
    .filter_map(|row| row.downcast::<adw::SwitchRow>().ok())
    .find(|row| row.title() == "Sounds on start, stop and error")
    .expect("a Sounds switch on General");
    if !sounds_row.is_active() {
        eprintln!("the Sounds switch starts off, though the schema defaults it on");
        return glib::ExitCode::FAILURE;
    }
    let stored_sounds = || {
        GioClientSettings::open()
            .ok()
            .and_then(|settings| settings.get(myna_core::settings::KEY_SOUNDS).ok())
    };
    sounds_row.set_active(false);
    if !settles(&|| stored_sounds() == Some(ClientSettingValue::Boolean(false))) {
        eprintln!("turning Sounds off stored {:?}", stored_sounds());
        return glib::ExitCode::FAILURE;
    }
    println!("sounds: the switch writes the setting");

    // A refused switch puts the radio back and says so in a toast whose
    // Details button opens the full report.
    let toast_texts = || {
        descendants(overlay.upcast_ref(), &|widget| {
            widget.type_().name() == "AdwToastWidget"
        })
        .iter()
        .flat_map(|toast| {
            descendants(toast, &|widget| widget.is::<gtk::Label>())
                .into_iter()
                .filter_map(|label| label.downcast::<gtk::Label>().ok())
                .filter(|label| label.is_visible())
                .map(|label| label.label().to_string())
        })
        .collect::<Vec<_>>()
    };
    let failed_title =
        gettextrs::gettext("Changing to {model} failed").replace("{model}", "Whisper");
    let details_label = gettextrs::gettext("Details");
    machine.deny_switches(true);
    adw::prelude::ActionRowExt::activate(&whisper_row());
    let announced = || toast_texts() == [failed_title.clone(), details_label.clone()];
    if !settles_unasked(&|| {
        machine.switches_attempted() == 4 && idle() && chosen() == ["Parakeet"] && announced()
    }) || asked.get()
    {
        eprintln!(
            "a refused switch left {:?} chosen after {} switches and toasts {:?}, asked first: {}",
            chosen(),
            machine.switches_attempted(),
            toast_texts(),
            asked.get()
        );
        return glib::ExitCode::FAILURE;
    }
    machine.deny_switches(false);
    let details_button = descendants(overlay.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some(details_label.as_str()))
    })
    .into_iter()
    .next()
    .and_then(|button| button.downcast::<gtk::Button>().ok())
    .expect("details button");
    details_button.emit_clicked();
    let report = || {
        window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
    };
    if !settles(&|| {
        report().is_some_and(|dialog| {
            dialog.heading().as_deref() == Some(failed_title.as_str())
                && dialog.details_text().contains("access denied")
        })
    }) {
        eprintln!("the Details button did not open the failure report");
        return glib::ExitCode::FAILURE;
    }
    // libadwaita 1.5 ignores a close that lands during the open animation.
    for _ in 0..8 {
        settle_gtk();
    }
    report().expect("failure report").force_close();
    if !settles(&|| window.visible_dialog().is_none()) {
        eprintln!("the failure report did not close");
        return glib::ExitCode::FAILURE;
    }
    quiesce();
    println!("model-group: a refused switch reverts with a toast");

    if view_stack
        .child_by_name("model")
        .map(|child| view_stack.page(&child).title())
        .is_none_or(|title| title.as_deref() != Some(gettextrs::gettext("Model").as_str()))
    {
        eprintln!("the second tab is not the Model tab");
        return glib::ExitCode::FAILURE;
    }

    // The Model tab shows the one active backend, and opening it reads it.
    view_stack.set_visible_child_name("model");

    let idle_entry = || {
        content()
            .and_then(|page| {
                find_descendant(&page, &|widget| {
                    widget.widget_name() == "myna-setting-sleep-idle-seconds"
                })
            })
            .and_then(|widget| widget.downcast::<adw::EntryRow>().ok())
    };
    if !settles(&|| idle_entry().is_some_and(|entry| entry.text() == "300")) {
        eprintln!("the active model's page never showed the snapshot it read");
        return glib::ExitCode::FAILURE;
    }
    println!("backend-snapshot: read");

    // A change applies on its own, with no question but polkit's.
    let apply_idle = |text: &str| {
        let entry = idle_entry().expect("idle entry");
        entry.set_text(text);
        entry.emit_by_name::<()>("apply", &[]);
    };
    let wrote = |value: &str| {
        machine.applied().iter().any(|operation| {
            operation
                .iter()
                .any(|argument| argument == &format!("sleep-idle-seconds={value}"))
        })
    };
    apply_idle("600");
    if !settles(&|| wrote("600") && idle_entry().is_some_and(|entry| entry.text() == "600"))
        || window.visible_dialog().is_some()
    {
        eprintln!(
            "the change was not applied and read back on its own; the machine ran {:?}",
            machine.applied()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("backend-apply: read back");

    let refused_title = gettextrs::gettext("Changing “{setting}” failed")
        .replace("{setting}", &gettextrs::gettext("Unload when idle"));
    machine.refuse_applies(true);
    apply_idle("900");
    if !settles(&|| {
        toast_texts() == [refused_title.clone(), details_label.clone()]
            && idle_entry().is_some_and(|entry| entry.text() == "600")
    }) {
        eprintln!(
            "a refused change left {:?} with toasts {:?}",
            idle_entry().map(|entry| entry.text()),
            toast_texts()
        );
        return glib::ExitCode::FAILURE;
    }
    machine.refuse_applies(false);
    println!("backend-apply: a refused change reverts with a toast");

    view_stack.set_visible_child_name("diagnostics");
    let report = || {
        diagnostics_nav
            .visible_page()
            .and_then(|page| page.downcast::<ui::DiagnosticsPage>().ok())
            .map(|page| {
                let buffer = page.report_view().buffer();
                buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string()
            })
            .unwrap_or_default()
    };
    if !settles(&|| report().contains("myna-parakeet")) {
        eprintln!(
            "diagnostics never reported the fixture backends:\n{}",
            report()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("diagnostics-report: lists backends");

    // Ctrl+R refreshes whichever tab is showing; General reads nothing.
    view_stack.set_visible_child_name("general");
    let reads = quiesce();
    ActionGroupExt::activate_action(&window, "refresh", None);
    if quiesce() != reads {
        eprintln!("refreshing the General tab read the machine");
        return glib::ExitCode::FAILURE;
    }
    if !application
        .actions_for_accel("<Control>r")
        .iter()
        .any(|action| action == "win.refresh")
    {
        eprintln!("Ctrl+R does not refresh");
        return glib::ExitCode::FAILURE;
    }
    for tab in ["diagnostics", "model"] {
        view_stack.set_visible_child_name(tab);
        let reads = quiesce();
        ActionGroupExt::activate_action(&window, "refresh", None);
        if !settles(&|| machine.reads() > reads) {
            eprintln!("refreshing the {tab} tab read nothing");
            return glib::ExitCode::FAILURE;
        }
    }
    println!("refresh-accelerator: refreshes the tab");

    // Set Up Dictation reopens the wizard over this window, never beside an
    // operation in flight, and closing it re-reads the machine.
    let wizard = || {
        application
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<ui::OnboardingWindow>().ok())
    };
    let setup_enabled = || {
        window
            .lookup_action("setup")
            .is_some_and(|action| action.is_enabled())
    };
    let Ok(operation) = ui
        .operation_coordinator()
        .begin(crate::operation_gate::OperationKind::BackendApply)
    else {
        eprintln!("the probe could not hold an operation open");
        return glib::ExitCode::FAILURE;
    };
    ActionGroupExt::activate_action(&window, "setup", None);
    settle_gtk();
    if wizard().is_some() {
        eprintln!("the wizard opened while an operation was in flight");
        return glib::ExitCode::FAILURE;
    }
    ui.operation_coordinator().complete(operation.token());
    ActionGroupExt::activate_action(&window, "setup", None);
    if !settles(&|| wizard().is_some()) {
        eprintln!("Set Up Dictation did not open the wizard");
        return glib::ExitCode::FAILURE;
    }
    let opened = wizard().expect("wizard");
    // Modal over this window, and not openable twice.
    if !opened.is_modal()
        || opened.transient_for().as_ref() != Some(window.upcast_ref())
        || setup_enabled()
    {
        eprintln!("the wizard is not the one modal wizard over the settings window");
        return glib::ExitCode::FAILURE;
    }
    let reads = quiesce();
    opened.close();
    if !settles(&|| machine.reads() > reads && setup_enabled()) {
        eprintln!("closing the wizard did not re-read the machine");
        return glib::ExitCode::FAILURE;
    }
    println!("setup: reopens the wizard");

    // A model removed behind the window's back: Diagnostics points at the
    // wizard, never at a command to paste.
    machine
        .myna_only
        .store(true, std::sync::atomic::Ordering::SeqCst);
    view_stack.set_visible_child_name("diagnostics");
    quiesce();
    std::thread::sleep(crate::diagnostics::REFRESH_DEBOUNCE);
    ActionGroupExt::activate_action(&window, "refresh", None);
    let diagnostics_widget = || {
        diagnostics_nav
            .visible_page()
            .map(|page| page.upcast::<gtk::Widget>())
    };
    // Mapped, not merely present: a refresh in flight rebuilds the page with
    // its setup group hidden.
    let leads_to_setup = || {
        diagnostics_widget().is_some_and(|page| {
            find_descendant(&page, &|widget| {
                widget.is_mapped()
                    && widget
                        .dynamic_cast_ref::<gtk::Actionable>()
                        .and_then(|actionable| actionable.action_name())
                        .as_deref()
                        == Some("win.setup")
            })
            .is_some()
        })
    };
    if !settles(&|| !report().contains("myna-parakeet") && leads_to_setup()) {
        eprintln!(
            "diagnostics of a machine with no model do not lead to Set Up Dictation:\n{}",
            report()
        );
        return glib::ExitCode::FAILURE;
    }
    if diagnostics_widget().is_some_and(|page| {
        find_descendant(&page, &|widget| {
            widget.is::<adw::ActionRow>() && widget.has_css_class("monospace")
        })
        .is_some()
    }) {
        eprintln!("diagnostics still show a command to paste");
        return glib::ExitCode::FAILURE;
    }
    let first_group = diagnostics_widget().and_then(|page| {
        descendants(&page, &|widget| {
            widget.is::<adw::PreferencesGroup>() && widget.is_visible()
        })
        .into_iter()
        .next()
        .and_then(|group| group.downcast::<adw::PreferencesGroup>().ok())
    });
    let Some(first_group) = first_group else {
        eprintln!("diagnostics page has no visible group");
        return glib::ExitCode::FAILURE;
    };
    let setup_button = find_descendant(first_group.upcast_ref(), &|widget| {
        widget.is::<gtk::Button>()
            && widget
                .dynamic_cast_ref::<gtk::Actionable>()
                .and_then(|actionable| actionable.action_name())
                .as_deref()
                == Some("win.setup")
    });
    let setup_row = find_descendant(first_group.upcast_ref(), &|widget| {
        widget.is::<adw::ActionRow>()
    })
    .and_then(|row| row.downcast::<adw::ActionRow>().ok());
    if setup_row.is_none() || first_group.title() != "No model installed" {
        eprintln!(
            "the setup group is not the first group on Diagnostics: {:?}",
            first_group.title()
        );
        return glib::ExitCode::FAILURE;
    }
    let marked = setup_button.as_ref().is_some_and(|button| {
        button.has_css_class("suggested-action")
            && setup_row
                .as_ref()
                .and_then(|row| row.activatable_widget())
                .as_ref()
                == Some(button)
    });
    if !marked {
        eprintln!("the setup row has no Set up button to mark it as the way on");
        return glib::ExitCode::FAILURE;
    }
    if setup_row.is_some_and(|row| {
        find_descendant(row.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::Image>()
                .and_then(|image| image.icon_name())
                .as_deref()
                == Some("go-next-symbolic")
        })
        .is_some()
    }) {
        eprintln!("the setup row promises a subpage it does not push");
        return glib::ExitCode::FAILURE;
    }
    println!("diagnostics-onboarding: leads to setup");

    ui.shutdown();
    window.close();
    glib::ExitCode::SUCCESS
}

/// Every action a menu model reaches, sections included, in order.
fn menu_actions(menu: &gio::MenuModel) -> Vec<String> {
    (0..menu.n_items())
        .flat_map(|index| {
            let action = menu
                .item_attribute_value(index, "action", None)
                .and_then(|value| value.get::<String>());
            let section = menu
                .item_link(index, "section")
                .map(|section| menu_actions(&section))
                .unwrap_or_default();
            action.into_iter().chain(section)
        })
        .collect()
}

/// Whether the component step heads itself as the design: a regular 24 px
/// title over the one paragraph, whatever is installed.
/// The component step's title, and the line under it saying whether anything
/// is still to install.
fn components_headed(window: &ui::OnboardingWindow, installed: bool) -> bool {
    let shown = |text: String, class: Option<&str>| {
        find_descendant(window.upcast_ref(), &|widget| {
            widget.downcast_ref::<gtk::Label>().is_some_and(|label| {
                label.is_mapped()
                    && label.label() == text.as_str()
                    && class.is_none_or(|class| styled(label, class))
            })
        })
        .is_some()
    };
    shown(
        gettextrs::gettext("Install components"),
        Some("onboarding-title"),
    ) && shown(
        if installed {
            gettextrs::gettext("Everything Dictation needs is installed.")
        } else {
            gettextrs::gettext("You need to install some components for Dictation to work.")
        },
        None,
    )
}

/// The extension row's Enable against a scripted gnome-shell: a failure
/// reverts with a toast whose report names the D-Bus call, the row shows
/// the enable under way, and enabling the last missing piece moves on.
fn probe_extension_enable(application: &adw::Application) -> Result<(), String> {
    use crate::onboarding::{assess, ComponentId, ExtensionState, Machine};

    let machine = ProbeMachine::new();
    let extensions = ProbeExtensions::new(ExtensionState::Disabled);
    let window = {
        let ui = crate::onboarding_ui::OnboardingUi::present_with_ports(
            application,
            assess(Machine {
                user_daemons: true,
                myna_installed: true,
                backend_discovered: true,
                extension: ExtensionState::Disabled,
                ..Machine::default()
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            extensions.clone(),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.set_beat(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let page = components_page(&window).ok_or("the component step shows no component page")?;
    let until = |done: &dyn Fn() -> bool| {
        for _ in 0..200 {
            if done() {
                break;
            }
            settle_gtk();
        }
        done()
    };
    let toast_texts = || {
        descendants(window.upcast_ref(), &|widget| {
            widget.type_().name() == "AdwToastWidget"
        })
        .iter()
        .flat_map(|toast| {
            descendants(toast, &|widget| widget.is::<gtk::Label>())
                .into_iter()
                .filter_map(|label| label.downcast::<gtk::Label>().ok())
                .map(|label| label.label().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
    };
    let step = || {
        window
            .navigation()
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    };
    let enable = gettextrs::gettext("Enable");
    let offers = |expected: &[&str]| rows_offer(&page) == expected;
    if !until(&|| offers(&["Installed", "Installed", &enable])) {
        return Err(format!(
            "a disabled extension offers {:?}",
            rows_offer(&page)
        ));
    }
    let button = page
        .row(ComponentId::ShellExtension)
        .expect("the extension row")
        .button;

    let reason = "gnome-shell could not run myna-shell@canonical.com: TypeError: boom";
    extensions.failing.replace(Some(reason.to_owned()));
    button.emit_clicked();
    let heading = gettextrs::gettext("Enabling the shell extension failed");
    if !until(&|| {
        toast_texts() == [heading.clone(), gettextrs::gettext("Details")]
            && offers(&["Installed", "Installed", &enable])
    }) || extensions.enables.get() != 1
    {
        return Err(format!(
            "a failed enable left {:?} with toasts {:?} after {} calls",
            rows_offer(&page),
            toast_texts(),
            extensions.enables.get()
        ));
    }
    let details = gettextrs::gettext("Details");
    descendants(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some(details.as_str()))
    })
    .into_iter()
    .next()
    .and_then(|button| button.downcast::<gtk::Button>().ok())
    .ok_or("the failure toast has no Details button")?
    .emit_clicked();
    for _ in 0..8 {
        settle_gtk();
    }
    let dialog = window
        .visible_dialog()
        .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
        .ok_or("Details opened no failure report")?;
    let expected = format!(
        "{} org.gnome.Shell.Extensions.EnableExtension(\"myna-shell@canonical.com\")\n{} {reason}",
        gettextrs::gettext("D-Bus call:"),
        gettextrs::gettext("Message:"),
    );
    if dialog.details_text() != expected {
        return Err(format!(
            "the failed enable's report reads {:?}",
            dialog.details_text()
        ));
    }
    dialog.force_close();
    if !until(&|| window.visible_dialog().is_none()) {
        return Err("the failure report did not close".to_owned());
    }
    println!("onboarding-extension: a failure reverts with a toast and its report");

    extensions.failing.replace(None);
    extensions.holding.set(true);
    button.emit_clicked();
    let enabling = gettextrs::gettext("Enabling…");
    if !until(&|| offers(&["Installed", "Installed", &enabling])) {
        return Err(format!("an enable under way shows {:?}", rows_offer(&page)));
    }
    button.emit_clicked();
    settle_gtk();
    if extensions.enables.get() != 2 {
        return Err(format!(
            "a second enable started beside the first: {} calls",
            extensions.enables.get()
        ));
    }
    println!("onboarding-extension: enabling shown in the row");

    extensions.holding.set(false);
    let enabled = gettextrs::gettext("Enabled");
    if !until(&|| offers(&["Installed", "Installed", &enabled])) {
        return Err(format!(
            "an enabled extension shows {:?}",
            rows_offer(&page)
        ));
    }
    // An instant click is not a download to wait out: the row confirms it,
    // and moving on is Next's.
    for _ in 0..8 {
        settle_gtk();
    }
    if step() != "components" {
        return Err(format!("enabling the extension moved on to {}", step()));
    }
    window.forward_button().emit_clicked();
    if !until(&|| step() == "shortcut") {
        return Err(format!(
            "Next after enabling stayed on {} showing {:?}",
            step(),
            rows_offer(&page)
        ));
    }
    println!("onboarding-extension: enabled in the row, and Next moved on");
    window.close();
    settle_gtk();
    Ok(())
}

/// The Install buttons against a scripted snapd: the row follows the change,
/// one install runs at a time, a dismissed prompt reverts silently, a failed
/// change with a toast whose report names the request, and an install
/// started elsewhere is followed rather than offered again.
fn probe_installs(application: &adw::Application) -> Result<(), String> {
    use crate::onboarding::{assess, ComponentId, ExtensionState, Machine};

    let machine = ProbeMachine::flagged();
    let extensions = ProbeExtensions::new(ExtensionState::Disabled);
    let window = {
        let ui = crate::onboarding_ui::OnboardingUi::present_with_ports(
            application,
            assess(Machine {
                user_daemons: true,
                extension: ExtensionState::Disabled,
                ..Machine::default()
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            extensions.clone(),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.set_beat(Duration::from_secs(60));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let page = components_page(&window).ok_or("the component step shows no component page")?;
    let until = |done: &dyn Fn() -> bool| {
        for _ in 0..200 {
            if done() {
                break;
            }
            settle_gtk();
        }
        done()
    };
    let toast_texts = || {
        descendants(window.upcast_ref(), &|widget| {
            widget.type_().name() == "AdwToastWidget"
        })
        .iter()
        .flat_map(|toast| {
            descendants(toast, &|widget| widget.is::<gtk::Label>())
                .into_iter()
                .filter_map(|label| label.downcast::<gtk::Label>().ok())
                .map(|label| label.label().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
    };
    let row = |id| page.row(id).expect("an installable row");
    let offers = |expected: &[&str]| rows_offer(&page) == expected;
    let enable = gettextrs::gettext("Enable");
    if !until(&|| offers(&["Install", "Install", &enable])) {
        return Err(format!("a flagged machine offers {:?}", rows_offer(&page)));
    }

    machine.answer_install(Err(crate::ports::SystemConfiguratorError::Cancelled));
    row(ComponentId::Myna).button.emit_clicked();
    if !until(&|| machine.installs().len() == 1 && offers(&["Install", "Install", &enable]))
        || !toast_texts().is_empty()
    {
        return Err(format!(
            "a dismissed install prompt left {:?}, toasts {:?}",
            rows_offer(&page),
            toast_texts()
        ));
    }
    println!("onboarding-install: a dismissed prompt reverts silently");

    machine.hold_install(true);
    row(ComponentId::Myna).button.emit_clicked();
    let asking = || {
        offers(&[&gettextrs::gettext("Installing…"), "Install", &enable])
            && !row(ComponentId::Model).button.is_sensitive()
            && row(ComponentId::ShellExtension).button.is_sensitive()
            && row(ComponentId::Myna).row.is_sensitive()
            && !window.forward_button().is_sensitive()
    };
    if !until(&asking) {
        return Err(format!(
            "while snapd asks, the rows offer {:?}, the model's Install sensitive {}",
            rows_offer(&page),
            row(ComponentId::Model).button.is_sensitive()
        ));
    }
    row(ComponentId::Model).button.emit_clicked();
    settle_gtk();
    if machine.installs() != ["myna", "myna"] {
        return Err(format!(
            "a second install started beside the first: {:?}",
            machine.installs()
        ));
    }
    println!("onboarding-install: one install at a time");

    row(ComponentId::ShellExtension).button.emit_clicked();
    if !until(&|| {
        extensions.enables.get() == 1
            && offers(&[&gettextrs::gettext("Installing…"), "Install", "Enabled"])
    }) {
        return Err(format!(
            "enabling beside an install left {:?} after {} calls",
            rows_offer(&page),
            extensions.enables.get()
        ));
    }
    println!("onboarding-install: the extension enables beside an install");

    machine.download(Some(42));
    machine.hold_install(false);
    let percent = gettextrs::gettext("Installing {percent}%").replace("{percent}", "42");
    if !until(&|| offers(&[&percent, "Install", "Enabled"])) {
        return Err(format!(
            "a download under way shows {:?}",
            rows_offer(&page)
        ));
    }
    println!("onboarding-install: the download's percentage shown");

    machine.download(None);
    if !until(&|| offers(&["Installed", "Install", "Enabled"]))
        || !row(ComponentId::Model).button.is_sensitive()
    {
        return Err(format!(
            "a finished install left the rows {:?}",
            rows_offer(&page)
        ));
    }
    println!("onboarding-install: installed once snapd is done");

    machine.fail_installs(Some(
        "cannot perform the following tasks:\n- Run install hook",
    ));
    row(ComponentId::Model).button.emit_clicked();
    let heading = gettextrs::gettext("Installing the speech-to-text model failed");
    if !until(&|| {
        toast_texts() == [heading.clone(), gettextrs::gettext("Details")]
            && offers(&["Installed", "Install", "Enabled"])
    }) {
        return Err(format!(
            "a failed install left {:?} with toasts {:?}",
            rows_offer(&page),
            toast_texts()
        ));
    }
    let details = gettextrs::gettext("Details");
    descendants(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some(details.as_str()))
    })
    .into_iter()
    .next()
    .and_then(|button| button.downcast::<gtk::Button>().ok())
    .ok_or("the failure toast has no Details button")?
    .emit_clicked();
    for _ in 0..8 {
        settle_gtk();
    }
    let dialog = window
        .visible_dialog()
        .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
        .ok_or("Details opened no failure report")?;
    let expected = format!(
        "{} POST /v2/snaps/myna-parakeet (install, latest/edge)\n{} 202\n{}\ncannot perform the following tasks:\n- Run install hook",
        gettextrs::gettext("Request:"),
        gettextrs::gettext("HTTP status:"),
        gettextrs::gettext("Message:"),
    );
    if dialog.details_text() != expected {
        return Err(format!(
            "the failed install's report reads {:?}",
            dialog.details_text()
        ));
    }
    dialog.force_close();
    if !until(&|| window.visible_dialog().is_none()) {
        return Err("the failure report did not close".to_owned());
    }
    println!("onboarding-install: a failed change reverts with a toast and its report");

    // Discovery finds the backend's slot before its install has fetched the
    // model: the row, and Next, wait for the change.
    machine.fail_installs(None);
    machine.download(Some(10));
    row(ComponentId::Model).button.emit_clicked();
    let downloading = gettextrs::gettext("Installing {percent}%").replace("{percent}", "10");
    if !until(&|| offers(&["Installed", &downloading, "Enabled"])) {
        return Err(format!("the model install shows {:?}", rows_offer(&page)));
    }
    machine.installed(crate::onboarding::RECOMMENDED_BACKEND_SNAP);
    let reads = machine.reads();
    if !until(&|| machine.reads() > reads + 4)
        || !offers(&["Installed", &downloading, "Enabled"])
        || window.forward_button().is_sensitive()
    {
        return Err(format!(
            "a backend found mid-install shows {:?}, Next sensitive {}",
            rows_offer(&page),
            window.forward_button().is_sensitive()
        ));
    }
    println!("onboarding-install: the model waits for its change");
    machine.download(None);
    if !until(&|| offers(&["Installed", "Installed", "Enabled"])) {
        return Err(format!(
            "the model install ended as {:?}",
            rows_offer(&page)
        ));
    }
    println!("onboarding-install: the model installed");
    window.close();
    settle_gtk();

    // A change snapd runs for a missing row, started in a terminal or by a
    // wizard since closed, is followed and not started again.
    let machine = ProbeMachine::flagged();
    machine.installing_elsewhere(crate::onboarding::MYNA_SNAP);
    machine.download(Some(7));
    let window = {
        let ui = crate::onboarding_ui::OnboardingUi::present_with_ports(
            application,
            assess(Machine {
                user_daemons: true,
                ..Machine::default()
            }),
            Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
                std::sync::Arc::new(machine.clone()),
            )),
            Rc::new(machine.clone()),
            ProbeExtensions::new(ExtensionState::Unavailable),
            crate::onboarding_ui::Opener::FirstRun,
        );
        ui.set_poll_interval(Duration::from_millis(50));
        ui.window()
    };
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let page = components_page(&window).ok_or("the component step shows no component page")?;
    let elsewhere = gettextrs::gettext("Installing {percent}%").replace("{percent}", "7");
    let followed = || rows_offer(&page) == [elsewhere.as_str(), "Install", "-"];
    if !until(&followed) || !machine.installs().is_empty() {
        return Err(format!(
            "an install started elsewhere shows {:?}, installs {:?}",
            rows_offer(&page),
            machine.installs()
        ));
    }
    machine.download(None);
    if !until(&|| rows_offer(&page) == ["Installed", "Install", "-"]) {
        return Err(format!(
            "an install followed to its end shows {:?}",
            rows_offer(&page)
        ));
    }
    println!("onboarding-install: an install started elsewhere is followed");
    window.close();
    settle_gtk();
    Ok(())
}

/// The flag's switch asks snapd, whose polkit prompt is the only question:
/// dismissing it reverts silently, a refusal with a toast, and success
/// unlocks the list. It never turns the flag off.
fn probe_flag_switch(application: &adw::Application) -> Result<(), String> {
    use crate::onboarding::{assess, Machine};

    let machine = ProbeMachine::bare();
    let window = crate::onboarding_ui::OnboardingUi::present_with_ports(
        application,
        assess(Machine::default()),
        Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(machine.clone()),
        )),
        Rc::new(machine.clone()),
        ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
        crate::onboarding_ui::Opener::FirstRun,
    )
    .window();
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let page = components_page(&window).ok_or("the component step shows no component page")?;
    let flag = page.flag_switch();
    let list = page.component_list();
    let toast_texts = || {
        descendants(window.upcast_ref(), &|widget| {
            widget.type_().name() == "AdwToastWidget"
        })
        .iter()
        .flat_map(|toast| {
            descendants(toast, &|widget| widget.is::<gtk::Label>())
                .into_iter()
                .filter_map(|label| label.downcast::<gtk::Label>().ok())
                .map(|label| label.label().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
    };
    let until = |done: &dyn Fn() -> bool| {
        for _ in 0..100 {
            if done() {
                break;
            }
            settle_gtk();
        }
        done()
    };
    let off = || !flag.is_active() && !flag.state() && !list.is_sensitive();

    machine.answer_flag(Err(crate::ports::SystemConfiguratorError::Cancelled));
    flag.activate();
    if !until(&|| machine.flag_writes() == 1 && off()) || !toast_texts().is_empty() {
        return Err(format!(
            "a dismissed prompt left the switch active {} state {} after {} writes, toasts {:?}",
            flag.is_active(),
            flag.state(),
            machine.flag_writes(),
            toast_texts()
        ));
    }
    println!("onboarding-flag: a dismissed prompt reverts silently");

    machine.answer_flag(Err(
        crate::ports::SystemConfiguratorError::snapd_authorization_denied(
            "PUT /v2/snaps/system/conf (experimental.user-daemons=true)",
            401,
            "access denied",
        ),
    ));
    flag.activate();
    let announced = || {
        toast_texts()
            == [
                gettextrs::gettext("Could not let Myna run in the background"),
                gettextrs::gettext("Details"),
            ]
    };
    if !until(&|| announced() && off()) || machine.flag_writes() != 2 {
        return Err(format!(
            "a refused prompt left the switch active {} state {} with toasts {:?}",
            flag.is_active(),
            flag.state(),
            toast_texts()
        ));
    }
    println!("onboarding-flag: a refused prompt reverts with a toast");

    let details = gettextrs::gettext("Details");
    let details_button = descendants(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some(details.as_str()))
    })
    .into_iter()
    .next()
    .and_then(|button| button.downcast::<gtk::Button>().ok())
    .ok_or("the refusal toast has no Details button")?;
    details_button.emit_clicked();
    let report = || {
        window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
    };
    // libadwaita 1.5 ignores a close that lands during the open animation.
    for _ in 0..8 {
        settle_gtk();
    }
    let dialog = report().ok_or("Details opened no failure report")?;
    let text = dialog.details_text();
    let expected = format!(
        "{} PUT /v2/snaps/system/conf (experimental.user-daemons=true)\n{} 401\n{} access denied",
        gettextrs::gettext("Request:"),
        gettextrs::gettext("HTTP status:"),
        gettextrs::gettext("Message:"),
    );
    if text != expected {
        return Err(format!("the refusal's report reads {text:?}"));
    }
    let label = |text: &str| {
        find_descendant(dialog.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                // Word joiners keep hyphenated names whole; they read as nothing.
                .is_some_and(|label| label.label().replace('\u{2060}', "") == text)
        })
    };
    let (Some(details), Some(copy)) = (label(&text), label(&gettextrs::gettext("Copy Details")))
    else {
        return Err("the failure report lacks its text or Copy Details".to_owned());
    };
    let top = |widget: &gtk::Widget| {
        widget
            .compute_point(&dialog, &gtk::graphene::Point::zero())
            .map_or(0.0, |point| point.y())
    };
    let gap = top(&copy) - top(&details) - details.height() as f32;
    if gap > 24.0 {
        return Err(format!(
            "the report leaves {gap} px between its text and Copy Details"
        ));
    }
    dialog.force_close();
    if !until(&|| window.visible_dialog().is_none()) {
        return Err("the failure report did not close".to_owned());
    }
    println!("onboarding-flag: a refusal's report names the snapd request");

    let long = ui::OperationErrorDialog::new("heading", "summary", &"line\n".repeat(200));
    long.present(Some(&window));
    for _ in 0..8 {
        settle_gtk();
    }
    let scrolls = find_descendant(long.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.label().starts_with("line\nline"))
    })
    .and_then(|details| details.ancestor(gtk::ScrolledWindow::static_type()))
    .is_some();
    if !scrolls || long.height() > window.height() {
        return Err(format!(
            "a long report is {} px tall in a {} px window, scrolling {scrolls}",
            long.height(),
            window.height()
        ));
    }
    long.force_close();
    if !until(&|| window.visible_dialog().is_none()) {
        return Err("the long report did not close".to_owned());
    }
    println!("onboarding-flag: a long report scrolls inside the window");

    machine.hold_flag(true);
    flag.activate();
    let row = page.flag_row();
    let spinner = page.flag_spinner();
    let pending = || {
        flag.is_active()
            && !flag.state()
            && spinner.is_mapped()
            && spinner.is_spinning()
            && row.is_sensitive()
            && !row.can_target()
            && !flag.can_target()
            && row.subtitle().as_deref() == Some(gettextrs::gettext("Enabling…").as_str())
            && !list.is_sensitive()
    };
    if !until(&pending) || machine.flag_writes() != 3 {
        return Err(format!(
            "while snapd asks, the switch is active {} state {}, spinner {}, subtitle {:?}",
            flag.is_active(),
            flag.state(),
            spinner.is_mapped(),
            row.subtitle()
        ));
    }
    // Activating the row again asks nothing more.
    adw::prelude::ActionRowExt::activate(&row);
    settle_gtk();
    if !pending() || machine.flag_writes() != 3 {
        return Err("activating the pending row asked snapd again".to_owned());
    }
    println!("onboarding-flag: pending while snapd asks");

    machine.hold_flag(false);
    let on = || {
        flag.is_active()
            && flag.state()
            && !spinner.is_visible()
            && row.can_target()
            && list.is_sensitive()
            && row.subtitle().as_deref()
                == Some(
                    gettextrs::gettext("Dictation needs it. You may be asked for your password.")
                        .as_str(),
                )
    };
    if !until(&on) || rows_offer(&page) != ["Install", "Install", "Enabled"] {
        return Err(format!(
            "turning the flag on left the switch state {}, the list sensitive {}, rows {:?}",
            flag.state(),
            list.is_sensitive(),
            rows_offer(&page)
        ));
    }
    println!("onboarding-flag: on, the list unlocked");

    flag.activate();
    settle_gtk();
    settle_gtk();
    if !on() || machine.flag_writes() != 3 {
        return Err("the switch turned the flag off".to_owned());
    }
    println!("onboarding-flag: stays on");
    window.close();
    settle_gtk();

    // A read that began before the flag was turned on answers after it.
    let machine = ProbeMachine::bare();
    let window = crate::onboarding_ui::OnboardingUi::present_with_ports(
        application,
        assess(Machine::default()),
        Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(machine.clone()),
        )),
        Rc::new(machine.clone()),
        ProbeExtensions::new(crate::onboarding::ExtensionState::Enabled),
        crate::onboarding_ui::Opener::FirstRun,
    )
    .window();
    settle_gtk();
    window.forward_button().emit_clicked();
    settle_gtk();
    let page = components_page(&window).ok_or("the component step shows no component page")?;
    let flag = page.flag_switch();
    machine.hold_reads(true);
    let elsewhere = gtk::Window::new();
    elsewhere.present();
    settle_gtk();
    elsewhere.close();
    window.present();
    if !until(&|| machine.flag_reads() == 1) {
        return Err("regaining focus did not read the flag".to_owned());
    }
    flag.activate();
    if !until(&|| machine.flag_writes() == 1) {
        return Err("the switch did not ask snapd".to_owned());
    }
    settle_gtk();
    machine.hold_reads(false);
    for _ in 0..40 {
        if !flag.is_active() {
            return Err("a read from before the flag turned the switch back off".to_owned());
        }
        if flag.state() {
            break;
        }
        settle_gtk();
    }
    if !flag.state() || !page.component_list().is_sensitive() {
        return Err("the read after the flag did not turn the switch on".to_owned());
    }
    println!("onboarding-flag: a stale read does not undo it");
    window.close();
    settle_gtk();
    Ok(())
}

fn components_page(window: &ui::OnboardingWindow) -> Option<ui::OnboardingComponents> {
    find_descendant(window.upcast_ref(), &|widget| {
        widget.is_mapped() && widget.is::<ui::OnboardingComponents>()
    })
    .and_then(|widget| widget.downcast().ok())
}

/// What each installable row shows at its end, in order: its button's label,
/// "Installed" for the check, or "-" for nothing.
fn rows_offer(page: &ui::OnboardingComponents) -> Vec<String> {
    use crate::onboarding::ComponentId;
    [
        ComponentId::Myna,
        ComponentId::Model,
        ComponentId::ShellExtension,
    ]
    .into_iter()
    .filter_map(|id| page.row(id))
    .map(|row| {
        let installed = row.installed.is_mapped()
            && find_descendant(row.installed.upcast_ref(), &|widget| {
                widget.has_css_class("success")
                    && widget.downcast_ref::<gtk::Image>().is_some_and(|image| {
                        image.icon_name().as_deref() == Some("object-select-symbolic")
                    })
            })
            .is_some();
        let installing = Some(&row.installing)
            .filter(|progress| progress.container.is_mapped() && progress.spinner.is_spinning());
        let done = || {
            find_descendant(row.installed.upcast_ref(), &|widget| {
                widget.is::<gtk::Label>()
            })
            .and_then(|label| label.downcast::<gtk::Label>().ok())
            .map(|label| label.label().to_string())
            .unwrap_or_default()
        };
        match (row.button.is_mapped(), installed, installing) {
            (false, false, Some(progress)) => progress.label.label().to_string(),
            (true, false, None) => row.button.label().unwrap_or_default().to_string(),
            (false, true, None) => done(),
            (false, false, None) => "-".to_owned(),
            _ => "both".to_owned(),
        }
    })
    .collect()
}

/// Whether `label`, or the balanced label holding it, has `class`.
fn styled(label: &gtk::Label, class: &str) -> bool {
    label.has_css_class(class)
        || label
            .parent()
            .is_some_and(|parent| parent.is::<ui::BalancedLabel>() && parent.has_css_class(class))
}

/// The widths of the lines the mapped label showing `text` wraps into.
fn shown_lines(window: &ui::OnboardingWindow, text: &str) -> Vec<i32> {
    find_descendant(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.is_mapped() && label.label() == text)
    })
    .and_then(|widget| widget.downcast::<gtk::Label>().ok())
    .map(|label| {
        let layout = label.layout();
        (0..layout.line_count())
            .filter_map(|line| layout.line_readonly(line))
            .map(|line| line.pixel_extents().1.width())
            .collect()
    })
    .unwrap_or_default()
}

/// The visible step's header bar: `None` unless it is flat and shows no
/// title, else whether it shows a back arrow.
fn header(window: &ui::OnboardingWindow) -> Option<bool> {
    let page = window.navigation().visible_page()?;
    let toolbar = page.child()?.downcast::<adw::ToolbarView>().ok()?;
    let bar = find_descendant(page.upcast_ref(), &|widget| {
        widget.is_mapped() && widget.is::<adw::HeaderBar>()
    })?
    .downcast::<adw::HeaderBar>()
    .ok()?;
    if bar.shows_title() || toolbar.top_bar_style() != adw::ToolbarStyle::Flat {
        return None;
    }
    Some(
        find_descendant(bar.upcast_ref(), &|widget| {
            widget.is_mapped() && widget.is::<gtk::Button>() && widget.has_css_class("back")
        })
        .is_some(),
    )
}

/// Whether each of `texts` shows on a single line.
fn one_line_each(window: &ui::OnboardingWindow, texts: &[&str]) -> bool {
    texts.iter().all(|text| {
        let lines = shown_lines(window, &gettextrs::gettext(*text));
        if lines.len() != 1 {
            eprintln!("{text:?} wraps into {lines:?}");
        }
        lines.len() == 1
    })
}

/// Whether the shortcut step heads itself as the design: a regular 24 px
/// title in the 540 px column.
fn shortcut_headed(window: &ui::OnboardingWindow) -> bool {
    find_descendant(window.upcast_ref(), &|widget| {
        widget.downcast_ref::<gtk::Label>().is_some_and(|label| {
            label.is_mapped()
                && label.label() == gettextrs::gettext("How to dictate").as_str()
                && styled(label, "onboarding-title")
                && label
                    .ancestor(adw::Clamp::static_type())
                    .and_then(|clamp| clamp.downcast::<adw::Clamp>().ok())
                    .is_some_and(|clamp| clamp.maximum_size() == 540)
        })
    })
    .is_some()
}

/// Whether the shortcut step shows a bound Super+J as the design: the one
/// sentence over its key caps.
fn shortcut_shown(window: &ui::OnboardingWindow) -> bool {
    let sentence =
        gettextrs::gettext("You can trigger Dictation anytime by using the keyboard shortcut:");
    let described = find_descendant(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.is_mapped() && label.label() == sentence.as_str())
    });
    described.is_some() && keycaps(window.upcast_ref()) == ["Super", "J"]
}

/// The labels of the mapped key caps under `root`, in order.
fn keycaps(root: &gtk::Widget) -> Vec<String> {
    let mut caps = Vec::new();
    if root.is_mapped() && root.has_css_class("keycap") {
        if let Some(label) = root.downcast_ref::<gtk::Label>() {
            caps.push(label.label().to_string());
        }
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        caps.extend(keycaps(&widget));
        child = widget.next_sibling();
    }
    caps
}

/// Whether every mapped key cap under `root` sits in a dimmed container.
fn keycaps_dimmed(root: &gtk::Widget) -> bool {
    if root.is_mapped() && root.has_css_class("keycap") {
        return false;
    }
    if root.has_css_class("dim-label") {
        return true;
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if !keycaps_dimmed(&widget) {
            return false;
        }
        child = widget.next_sibling();
    }
    true
}

/// Whether the onboarding footer says everything is installed: a success
/// checkmark and the label, left of the forward button. `None` when it is not
/// shown at all.
/// Whether the footer spins while dictation is being set up.
fn setup_spinner(window: &ui::OnboardingWindow) -> bool {
    let Some(footer) = window.forward_button().parent() else {
        return false;
    };
    find_descendant(&footer, &|widget| {
        widget
            .downcast_ref::<gtk::Spinner>()
            .is_some_and(|spinner| spinner.is_mapped() && spinner.is_spinning())
    })
    .is_some()
}

/// A stock libadwaita button at its natural size: `suggested-action` when it
/// is the step's main action, no styling otherwise.
fn standard_button(button: &gtk::Button, main: bool) -> bool {
    let styled = button
        .css_classes()
        .iter()
        .any(|class| !matches!(class.as_str(), "suggested-action" | "text-button"));
    button.has_css_class("suggested-action") == main && !styled && button.width_request() == -1
}

/// What the fixture's refused restart says, for its report.
const PROBE_RESTART_FAILURE: &str = "Job for snap.myna.myna.service failed";

fn toast_texts(window: &ui::OnboardingWindow) -> Vec<String> {
    descendants(window.upcast_ref(), &|widget| {
        widget.type_().name() == "AdwToastWidget"
    })
    .iter()
    .flat_map(|toast| {
        descendants(toast, &|widget| widget.is::<gtk::Label>())
            .into_iter()
            .filter_map(|label| label.downcast::<gtk::Label>().ok())
            .map(|label| label.label().to_string())
            .collect::<Vec<_>>()
    })
    .collect()
}

fn setup_failure_toasted(window: &ui::OnboardingWindow) -> bool {
    toast_texts(window)
        == [
            gettextrs::gettext("Could not set up Dictation"),
            gettextrs::gettext("Details"),
        ]
}

/// Open the failure toast's Details and read the report, closing it again.
fn setup_failure_report(window: &ui::OnboardingWindow) -> Option<String> {
    let details = gettextrs::gettext("Details");
    descendants(window.upcast_ref(), &|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some(details.as_str()))
    })
    .into_iter()
    .next()?
    .downcast::<gtk::Button>()
    .ok()?
    .emit_clicked();
    for _ in 0..8 {
        settle_gtk();
    }
    let dialog = window
        .visible_dialog()?
        .downcast::<ui::OperationErrorDialog>()
        .ok()?;
    let report = dialog.details_text();
    dialog.force_close();
    for _ in 0..8 {
        settle_gtk();
    }
    Some(report.to_string())
}

/// The footer says a setup left dictation not set up, with a warning icon.
fn setup_failed_shown(window: &ui::OnboardingWindow) -> bool {
    let status = window.setup_failed_status();
    status.is_mapped()
        && find_descendant(status.upcast_ref(), &|widget| {
            widget.downcast_ref::<gtk::Label>().is_some_and(|label| {
                label.label()
                    == gettextrs::gettext("Dictation is not set up yet. Select Next to try again.")
                        .as_str()
            })
        })
        .is_some()
        && find_descendant(status.upcast_ref(), &|widget| {
            widget.has_css_class("warning")
        })
        .is_some()
}

fn installed_status(window: &ui::OnboardingWindow) -> Option<bool> {
    let label = find_descendant(window.upcast_ref(), &|widget| {
        widget.downcast_ref::<gtk::Label>().is_some_and(|label| {
            label.is_mapped()
                && label.label() == gettextrs::gettext("All required components installed").as_str()
        })
    })?;
    let check = label.parent().and_then(|status| {
        find_descendant(&status, &|widget| {
            widget.is_mapped()
                && widget.has_css_class("success")
                && widget.downcast_ref::<gtk::Image>().is_some_and(|image| {
                    image.icon_name().as_deref() == Some("object-select-symbolic")
                })
        })
    });
    let forward = window.forward_button();
    let left_of_forward = label
        .compute_bounds(&forward)
        .is_some_and(|bounds| bounds.x() + bounds.width() <= 0.0);
    Some(check.is_some() && left_of_forward)
}

fn find_descendant(
    widget: &gtk::Widget,
    matches: &dyn Fn(&gtk::Widget) -> bool,
) -> Option<gtk::Widget> {
    if matches(widget) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_descendant(&current, matches) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// Every descendant of `widget` that `matches`, in tree order.
fn descendants(widget: &gtk::Widget, matches: &dyn Fn(&gtk::Widget) -> bool) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    if matches(widget) {
        found.push(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        found.extend(descendants(&current, matches));
        child = current.next_sibling();
    }
    found
}

fn first_entry_row(widget: &gtk::Widget) -> Option<adw::EntryRow> {
    find_descendant(widget, &|widget| widget.is::<adw::EntryRow>())
        .and_then(|widget| widget.downcast().ok())
}

fn settle_gtk() {
    let context = glib::MainContext::default();
    for _ in 0..20 {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Rows placed with the active model go into `model_settings`, which the
/// Model tab shows.
fn build_myna_page(
    controller: Rc<MynaSettingsController>,
    writer: PersistenceWriter,
    overlay: &adw::ToastOverlay,
    model_settings: &adw::PreferencesGroup,
) -> adw::NavigationPage {
    match controller.state() {
        PageState::Loading => status_page(
            &gettextrs::gettext("Loading Myna Settings"),
            &gettextrs::gettext("Reading the installed settings schema…"),
            "content-loading-symbolic",
        ),
        PageState::Empty => status_page(
            &gettextrs::gettext("No Myna Settings"),
            &gettextrs::gettext("The installed schema does not declare any settings."),
            "edit-clear-all-symbolic",
        ),
        PageState::Error(message) => error_page(&message),
        PageState::Ready(rows) => ready_page(controller, writer, rows, overlay, model_settings),
    }
}

fn status_page(title: &str, description: &str, icon: &str) -> adw::NavigationPage {
    let page = ui::StatusPage::new();
    page.set_status(title, description, icon);
    page.upcast()
}

/// The page in place of settings that could not load: a plain sentence, the
/// raw error under Details and in the log.
fn error_page(detail: &str) -> adw::NavigationPage {
    glib::g_warning!(crate::LOG_DOMAIN, "settings unavailable: {detail}");
    let page = ui::StatusPage::new();
    page.set_status(
        &gettextrs::gettext("Myna Settings Unavailable"),
        &gettextrs::gettext("Myna Settings could not start."),
        "dialog-error-symbolic",
    );
    page.set_details(detail);
    page.upcast()
}

#[derive(Clone)]
enum RowBinding {
    Choice {
        row: adw::ComboRow,
        choices: Vec<String>,
        writable: bool,
        updating: Rc<Cell<bool>>,
    },
    Number {
        row: adw::SpinRow,
        writable: bool,
        updating: Rc<Cell<bool>>,
    },
    Switch {
        row: adw::SwitchRow,
        writable: bool,
        updating: Rc<Cell<bool>>,
    },
    Text {
        row: adw::EntryRow,
        key: String,
        writable: bool,
        updating: Rc<Cell<bool>>,
        commit: Rc<RefCell<DebouncedTextCommit>>,
        source: Rc<RefCell<Option<glib::SourceId>>>,
        controller: Rc<MynaSettingsController>,
        writer: PersistenceWriter,
    },
}

impl Drop for RowBinding {
    fn drop(&mut self) {
        if let Self::Text {
            key,
            commit,
            source,
            controller,
            writer,
            ..
        } = self
        {
            cancel_source(source);
            if let Some(value) = commit.borrow_mut().flush() {
                if let Ok(request) = controller.set(key, ClientSettingValue::Text(value)) {
                    persist_request(writer.clone(), controller.clone(), request, None);
                }
            }
        }
    }
}

impl RowBinding {
    /// `pending` is deliberately not wired to sensitivity. Desensitizing a row
    /// while its own write is in flight yanks focus out of the entry the user
    /// is still typing in (and GTK warns that the GtkText never got a
    /// focus-out). The write is already ordered by the controller's revision
    /// gate, so nothing needed the lockout.
    fn apply(&self, value: &ClientSettingValue, _pending: bool) {
        match self {
            Self::Choice {
                row,
                choices,
                writable,
                updating,
            } => {
                updating.set(true);
                if let Some(value) = value.as_str() {
                    if let Some(index) = choices.iter().position(|choice| choice == value) {
                        row.set_selected(index as u32);
                    }
                }
                row.set_sensitive(*writable);
                updating.set(false);
            }
            Self::Number {
                row,
                writable,
                updating,
            } => {
                updating.set(true);
                if let Some(value) = value.as_integer() {
                    row.set_value(value as f64);
                }
                row.set_sensitive(*writable);
                updating.set(false);
            }
            Self::Switch {
                row,
                writable,
                updating,
            } => {
                updating.set(true);
                if let Some(value) = value.as_bool() {
                    row.set_active(value);
                }
                row.set_sensitive(*writable);
                updating.set(false);
            }
            Self::Text {
                row,
                writable,
                updating,
                commit,
                source,
                ..
            } => {
                cancel_source(source);
                commit
                    .borrow_mut()
                    .committed(value.as_str().unwrap_or_default());
                updating.set(true);
                let text = value.as_str().unwrap_or_default();
                // Re-setting identical text still moves the cursor to the end.
                if row.text() != text {
                    row.set_text(text);
                }
                row.set_sensitive(*writable);
                updating.set(false);
            }
        }
    }
}

/// No visible subtitle: the schema description is exposed to assistive tech
/// only, keeping the row a single line.
fn describe(row: &impl IsA<gtk::Widget>, description: &str) {
    let row = row.as_ref();
    row.update_property(&[gtk::accessible::Property::Description(description)]);
}

fn ready_page(
    controller: Rc<MynaSettingsController>,
    writer: PersistenceWriter,
    rows: Vec<SettingRow>,
    overlay: &adw::ToastOverlay,
    model_settings: &adw::PreferencesGroup,
) -> adw::NavigationPage {
    let page = ui::MynaPage::new();
    crate::shortcut_ui::ShortcutControl::attach(
        page.shortcut_keys(),
        page.shortcut_button(),
        overlay.clone(),
        crate::shortcut_ui::Surface::Row,
        Box::new({
            let row = page.shortcut_row();
            move |state, _| row.set_subtitle(&crate::shortcut_ui::row_subtitle(state))
        }),
    );
    let dictation = page.settings_group();
    let bindings = Rc::new(RefCell::new(BTreeMap::<String, RowBinding>::new()));

    for setting in rows {
        let plan = widget_plan(setting.metadata());
        let group = match plan.placement {
            Placement::Dictation => &dictation,
            Placement::ActiveModel => model_settings,
        };
        let widget_name = (plan.placement == Placement::ActiveModel)
            .then(|| crate::backend_ui::client_setting_widget_name(&plan.key));
        let add = |row: &gtk::Widget| {
            if let Some(name) = &widget_name {
                row.set_widget_name(name);
            }
            group.add(row);
        };
        match plan.kind {
            WidgetKind::Choice => {
                let display_labels: Vec<_> = plan
                    .choices
                    .iter()
                    .map(|choice| choice_display_label(choice))
                    .collect();
                let labels: Vec<&str> = display_labels.iter().map(String::as_str).collect();
                let model = gtk::StringList::new(&labels);
                let row = adw::ComboRow::builder()
                    .title(&plan.title)
                    .model(&model)
                    .sensitive(plan.writable)
                    .build();
                describe(&row, &plan.description);
                if let Some(index) = controller
                    .shown_value(&plan.key)
                    .as_ref()
                    .and_then(ClientSettingValue::as_str)
                    .and_then(|value| plan.choices.iter().position(|choice| choice == value))
                {
                    row.set_selected(index as u32);
                }
                let updating = Rc::new(Cell::new(false));
                row.connect_selected_notify({
                    let controller = controller.clone();
                    let key = plan.key.clone();
                    let choices = plan.choices.clone();
                    let updating = updating.clone();
                    let writer = writer.clone();
                    move |row| {
                        if !updating.get() {
                            if let Some(value) = choices.get(row.selected() as usize) {
                                if let Ok(request) =
                                    controller.set(&key, ClientSettingValue::Choice(value.clone()))
                                {
                                    persist_request(
                                        writer.clone(),
                                        controller.clone(),
                                        request,
                                        None,
                                    );
                                }
                            }
                        }
                    }
                });
                bindings.borrow_mut().insert(
                    plan.key.clone(),
                    RowBinding::Choice {
                        row: row.clone(),
                        choices: plan.choices,
                        writable: plan.writable,
                        updating,
                    },
                );
                add(row.upcast_ref());
            }
            WidgetKind::Number => {
                let (minimum, maximum) = plan.bounds.expect("Number plans carry bounds");
                let row = adw::SpinRow::with_range(minimum as f64, maximum as f64, 1.0);
                row.set_title(&plan.title);
                row.set_sensitive(plan.writable);
                row.set_value(setting.value().as_integer().unwrap_or(minimum) as f64);
                describe(&row, &plan.description);
                let updating = Rc::new(Cell::new(false));
                row.connect_value_notify({
                    let controller = controller.clone();
                    let key = plan.key.clone();
                    let updating = updating.clone();
                    let writer = writer.clone();
                    move |row| {
                        if updating.get() {
                            return;
                        }
                        let value = row.value().round() as i64;
                        if let Ok(request) =
                            controller.set(&key, ClientSettingValue::Integer(value))
                        {
                            persist_request(writer.clone(), controller.clone(), request, None);
                        }
                    }
                });
                bindings.borrow_mut().insert(
                    plan.key.clone(),
                    RowBinding::Number {
                        row: row.clone(),
                        writable: plan.writable,
                        updating,
                    },
                );
                add(row.upcast_ref());
            }
            WidgetKind::Switch => {
                let row = adw::SwitchRow::builder()
                    .title(&plan.title)
                    .active(setting.value().as_bool().unwrap_or_default())
                    .sensitive(plan.writable)
                    .build();
                describe(&row, &plan.description);
                let updating = Rc::new(Cell::new(false));
                row.connect_active_notify({
                    let controller = controller.clone();
                    let key = plan.key.clone();
                    let updating = updating.clone();
                    let writer = writer.clone();
                    move |row| {
                        if updating.get() {
                            return;
                        }
                        if let Ok(request) =
                            controller.set(&key, ClientSettingValue::Boolean(row.is_active()))
                        {
                            persist_request(writer.clone(), controller.clone(), request, None);
                        }
                    }
                });
                bindings.borrow_mut().insert(
                    plan.key.clone(),
                    RowBinding::Switch {
                        row: row.clone(),
                        writable: plan.writable,
                        updating,
                    },
                );
                add(row.upcast_ref());
            }
            WidgetKind::Text => {
                let row = adw::EntryRow::builder()
                    .title(&plan.title)
                    .text(setting.value().as_str().unwrap_or_default())
                    .sensitive(plan.writable)
                    .show_apply_button(true)
                    .build();
                describe(&row, &plan.description);
                let updating = Rc::new(Cell::new(false));
                let commit = Rc::new(RefCell::new(DebouncedTextCommit::new(
                    setting.value().as_str().unwrap_or_default(),
                )));
                let source = Rc::new(RefCell::new(None));
                row.connect_changed({
                    let controller = controller.clone();
                    let key = plan.key.clone();
                    let updating = updating.clone();
                    let commit = commit.clone();
                    let source = source.clone();
                    let writer = writer.clone();
                    move |changed_row| {
                        if updating.get() {
                            return;
                        }
                        cancel_source(&source);
                        let Some(revision) =
                            commit.borrow_mut().changed(changed_row.text().as_str())
                        else {
                            return;
                        };
                        let controller = controller.clone();
                        let key = key.clone();
                        let commit = commit.clone();
                        let source_slot = source.clone();
                        let writer = writer.clone();
                        let hold =
                            gio::Application::default().map(|application| application.hold());
                        *source.borrow_mut() = Some(glib::timeout_add_local_once(
                            std::time::Duration::from_millis(250),
                            move || {
                                source_slot.borrow_mut().take();
                                let Some(value) = commit.borrow_mut().take(revision) else {
                                    return;
                                };
                                if let Ok(request) =
                                    controller.set(&key, ClientSettingValue::Text(value))
                                {
                                    persist_request(writer, controller, request, hold);
                                }
                            },
                        ));
                    }
                });
                row.connect_apply({
                    let controller = controller.clone();
                    let key = plan.key.clone();
                    let updating = updating.clone();
                    let commit = commit.clone();
                    let source = source.clone();
                    let writer = writer.clone();
                    move |row| {
                        if updating.get() {
                            return;
                        }
                        cancel_source(&source);
                        let value = row.text().to_string();
                        let Some(value) = commit.borrow_mut().apply(&value) else {
                            return;
                        };
                        if let Ok(request) = controller.set(&key, ClientSettingValue::Text(value)) {
                            persist_request(writer.clone(), controller.clone(), request, None);
                        }
                    }
                });
                bindings.borrow_mut().insert(
                    plan.key.clone(),
                    RowBinding::Text {
                        row: row.clone(),
                        key: plan.key.clone(),
                        writable: plan.writable,
                        updating,
                        commit,
                        source,
                        controller: controller.clone(),
                        writer: writer.clone(),
                    },
                );
                add(row.upcast_ref());
            }
        }
    }
    page.connect_map({
        let bindings = bindings.clone();
        move |_| {
            let _ = &bindings;
        }
    });

    controller.observe({
        let bindings = Rc::downgrade(&bindings);
        let overlay = overlay.downgrade();
        let observed_controller = Rc::downgrade(&controller);
        move |event| {
            let Some(bindings) = bindings.upgrade() else {
                return;
            };
            match event {
                SettingsEvent::RowChanged {
                    key,
                    value,
                    pending,
                } => {
                    if let Some(binding) = bindings.borrow().get(&key) {
                        binding.apply(&value, pending);
                    }
                }
                SettingsEvent::SaveFailed { key, detail } => {
                    if let Some(overlay) = overlay.upgrade() {
                        overlay.add_toast(adw::Toast::new(&format!(
                            "{}: {detail}",
                            gettextrs::gettext("Could not save the setting")
                        )));
                    }
                    if let Some(binding) = bindings.borrow().get(&key) {
                        if let Some(controller) = observed_controller.upgrade() {
                            binding.apply(
                                &controller
                                    .shown_value(&key)
                                    .expect("event refers to an existing row"),
                                false,
                            );
                        }
                    }
                }
            }
        }
    });

    page.upcast()
}

fn cancel_source(source: &RefCell<Option<glib::SourceId>>) {
    if let Some(source) = source.borrow_mut().take() {
        source.remove();
    }
}

fn persist_request(
    writer: PersistenceWriter,
    controller: Rc<MynaSettingsController>,
    request: PersistenceRequest,
    hold: Option<gio::ApplicationHoldGuard>,
) {
    let hold = hold.or_else(|| gio::Application::default().map(|application| application.hold()));
    let job = writer.submit(request.clone());
    glib::spawn_future_local(async move {
        let result = match job {
            Ok(job) => gio::spawn_blocking(move || job.wait())
                .await
                .unwrap_or_else(|_| {
                    Err(ClientSettingsError::StoreUnavailable {
                        message: "the settings persistence worker terminated unexpectedly".into(),
                    })
                }),
            Err(error) => Err(error),
        };
        controller.complete(request, result);
        drop(hold);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_environment_values_are_explicit() {
        assert!(smoke_requested(Some(std::ffi::OsStr::new("1"))));
        assert!(smoke_requested(Some(std::ffi::OsStr::new("TRUE"))));
        assert!(!smoke_requested(Some(std::ffi::OsStr::new("yes"))));
        assert!(!smoke_requested(None));
    }
}
