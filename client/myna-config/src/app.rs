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
    choice_display_label, DebouncedTextCommit, MynaSettingsController, PageState,
    PersistenceRequest, PersistenceWriter, SettingRow, SettingsEvent,
};
use crate::onboarding::{assess, needs_onboarding, Machine};
use crate::ports::{ClientSettings, ClientSettingsError};
use crate::ui;
use crate::APP_ID;

pub use crate::myna_settings::{widget_plan, WidgetKind, WidgetPlan};

const SMOKE_ENV: &str = "MYNA_CONFIG_SMOKE_BUILD";
const TEMPLATE_ENV: &str = "MYNA_CONFIG_TEMPLATE_TEST";
const ACCESSIBILITY_ENV: &str = "MYNA_CONFIG_ACCESSIBILITY_TEST";
const TYPING_ENV: &str = "MYNA_CONFIG_TYPING_TEST";
const ONBOARDING_ENV: &str = "MYNA_CONFIG_ONBOARDING_TEST";
const SHORTCUT_ENV: &str = "MYNA_CONFIG_SHORTCUT_TEST";
const BACKENDS_ENV: &str = "MYNA_CONFIG_BACKENDS_TEST";
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

pub fn smoke_build(settings: Rc<dyn ClientSettings>) -> Result<Vec<WidgetPlan>, String> {
    settings
        .list()
        .map_err(|error| error.to_string())?
        .iter()
        .map(|metadata| {
            let plan = widget_plan(metadata);
            if plan.title.trim().is_empty()
                || (plan.kind == WidgetKind::Choice && plan.choices.is_empty())
                || (plan.kind == WidgetKind::Number && plan.bounds.is_none())
            {
                Err(format!("{} has incomplete widget metadata", plan.key))
            } else {
                Ok(plan)
            }
        })
        .collect()
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

    if smoke_requested(std::env::var_os(SHORTCUT_ENV).as_deref()) {
        return shortcut_probe();
    }

    if smoke_requested(std::env::var_os(BACKENDS_ENV).as_deref()) {
        return backends_probe();
    }

    if smoke_requested(std::env::var_os(SMOKE_ENV).as_deref()) {
        return match GioClientSettings::open()
            .map(|settings| Rc::new(settings) as Rc<dyn ClientSettings>)
            .map_err(|error| error.to_string())
            .and_then(smoke_build)
        {
            Ok(plans) => {
                println!("validated {} Myna settings widgets", plans.len());
                glib::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("myna-config smoke build failed: {error}");
                glib::ExitCode::FAILURE
            }
        };
    }

    ui::register_resources();
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build_window);
    application.run_with_args::<&str>(&[])
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

    let application = adw::Application::builder()
        .application_id(probe_app_id())
        .build();
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
    .unwrap_or_default();
    if [
        r#"<property name="label" translatable="yes">Refresh diagnostics</property>"#,
        r#"<property name="label" translatable="yes">Copy diagnostics</property>"#,
        r#"<property name="label" translatable="yes">Diagnostic report</property>"#,
        r#"<property name="description" translatable="yes">Re-read the machine, the daemon, and every backend.</property>"#,
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

    window.close();
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
    let application = adw::Application::builder()
        .application_id(probe_app_id())
        .build();
    let _ = application.register(None::<&gio::Cancellable>);

    let step = |window: &ui::OnboardingWindow| {
        window
            .stack()
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_default()
    };

    // A machine with nothing installed: the flow opens, and its component step
    // refuses to advance.
    let (window, start_button) = {
        let ui = OnboardingUi::present(&application, assess(Machine::default()), Box::new(|| {}));
        (ui.window(), ui.start_button())
    };
    settle_gtk();
    if step(&window) != "welcome" {
        eprintln!("the wizard did not open on its first step");
        return glib::ExitCode::FAILURE;
    }
    start_button.emit_clicked();
    settle_gtk();
    if step(&window) != "components" {
        eprintln!("activating the welcome button did not reach the component step");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-start: advanced");

    let forward = window.forward_button();
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
    window.close();
    settle_gtk();

    // A machine missing only the optional extension walks to the end, and
    // finishing hands control back to the caller.
    let installed = [crate::diagnostics::InstalledSnap {
        name: crate::onboarding::MYNA_SNAP.to_owned(),
        version: "1".to_owned(),
    }];
    let completed = Rc::new(Cell::new(false));
    let (window, start_button, shortcut_button) = {
        let ui = OnboardingUi::present(
            &application,
            assess(Machine::new(&installed, 1, false)),
            Box::new({
                let completed = completed.clone();
                move || completed.set(true)
            }),
        );
        (ui.window(), ui.start_button(), ui.shortcut_button())
    };
    settle_gtk();
    start_button.emit_clicked();
    settle_gtk();
    let forward = window.forward_button();
    if !forward.is_sensitive() {
        eprintln!("the component step refused to advance with only the extension missing");
        return glib::ExitCode::FAILURE;
    }
    forward.emit_clicked();
    settle_gtk();
    if step(&window) != "shortcut" {
        eprintln!("the component step did not reach the shortcut step");
        return glib::ExitCode::FAILURE;
    }
    let back = window.back_button();
    if !back.is_visible() {
        eprintln!("the shortcut step offers no way back");
        return glib::ExitCode::FAILURE;
    }
    back.emit_clicked();
    settle_gtk();
    if step(&window) != "components" {
        eprintln!("the back button did not return to the component step");
        return glib::ExitCode::FAILURE;
    }
    forward.emit_clicked();
    settle_gtk();
    println!("onboarding-walk: reached the last step");

    // No daemon runs under the probe, and nothing can bind a key without one.
    if shortcut_button.is_sensitive() {
        eprintln!("the shortcut step offered to bind a key with no daemon to bind it");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-shortcut: waits for the daemon");

    forward.emit_clicked();
    settle_gtk();
    if !completed.get() {
        eprintln!("finishing the wizard did not hand control back");
        return glib::ExitCode::FAILURE;
    }
    println!("onboarding-finish: handed back");
    glib::ExitCode::SUCCESS
}

fn template_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config template probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }

    let application = adw::Application::builder()
        .application_id(probe_app_id())
        .build();
    let _ = application.register(None::<&gio::Cancellable>);
    for resource in [
        "active-backend-dialog.ui",
        "apply-dialog.ui",
        "backend-apply-controls.ui",
        "backend-page.ui",
        "diagnostics-page.ui",
        "main-window.ui",
        "myna-page.ui",
        "onboarding-components.ui",
        "onboarding-shortcut.ui",
        "onboarding-welcome.ui",
        "onboarding-window.ui",
        "operation-error-dialog.ui",
        "sidebar-row.ui",
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
    let _switch = ui::ActiveBackendDialog::new("preview");
    println!("ActiveBackendDialog");
    let _apply = ui::ApplyDialog::new("preview");
    println!("ApplyDialog");
    let controls = ui::BackendApplyControls::new();
    let _ = (
        controls.progress_spinner(),
        controls.button_box(),
        controls.revert_button(),
        controls.apply_button(),
    );
    println!("BackendApplyControls");
    let myna = ui::MynaPage::new();
    let _ = (
        myna.preferences_page(),
        myna.active_backend_group(),
        myna.active_backend_row(),
        myna.switch_backend_button(),
        myna.settings_group(),
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
    let welcome = ui::OnboardingWelcome::new();
    let _ = (welcome.status(), welcome.start_button());
    println!("OnboardingWelcome");
    let components = ui::OnboardingComponents::new();
    let _ = (components.subtitle(), components.list());
    println!("OnboardingComponents");
    let shortcut = ui::OnboardingShortcut::new();
    let _ = (
        shortcut.description(),
        shortcut.shortcut_box(),
        shortcut.shortcut_button(),
    );
    println!("OnboardingShortcut");
    let onboarding = ui::OnboardingWindow::new(&application);
    let _ = (
        onboarding.overlay(),
        onboarding.window_title(),
        onboarding.stack(),
        onboarding.back_button(),
        onboarding.forward_button(),
    );
    println!("OnboardingWindow");
    let status = ui::StatusPage::new();
    let _ = status.status();
    println!("StatusPage");
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
        let components = assess_machine().await;
        if needs_onboarding(&components) {
            let settings_application = application.clone();
            crate::onboarding_ui::OnboardingUi::present(
                &application,
                components,
                Box::new(move || build_settings_window(&settings_application)),
            );
        } else {
            build_settings_window(&application);
        }
        drop(hold);
    });
}

/// One assessment of what dictation is missing on this machine. A surface that
/// cannot be read counts as nothing found, which opens the wizard: the flow
/// then shows what it could not verify rather than a settings window with no
/// backends and no explanation.
async fn assess_machine() -> Vec<crate::onboarding::Component> {
    use crate::adapters::snap_backend::SnapBackendRepository;
    use crate::command::{CancellationToken, GioCommandRunner};
    use crate::ports::BackendRepository;

    let repository = SnapBackendRepository::new(std::sync::Arc::new(GioCommandRunner));
    let installed = repository
        .installed_snaps(CancellationToken::new())
        .await
        .unwrap_or_default();
    let backends = repository
        .discover(CancellationToken::new())
        .await
        .map(|snapshot| snapshot.backends().len())
        .unwrap_or_default();
    assess(Machine::new(
        &installed,
        backends,
        crate::onboarding_ui::shell_extension_installed(),
    ))
}

fn build_settings_window(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }

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
        &gettextrs::gettext("Backend"),
        &gettextrs::gettext("Backend details will appear after discovery."),
        "content-loading-symbolic",
    )]);
    let diagnostics_page = status_page(
        &gettextrs::gettext("About and Diagnostics"),
        &gettextrs::gettext("Backend diagnostics will appear after discovery."),
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
            let myna_page = match GioClientSettings::open() {
                Ok(settings) => {
                    let writer = PersistenceWriter::spawn(GioClientSettings::open);
                    let controller =
                        MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>);
                    build_myna_page(controller, writer, &overlay)
                }
                Err(error) => error_page(&error.to_string()),
            };
            general_nav.replace(std::slice::from_ref(&myna_page));

            let ui = crate::backend_ui::BackendUi::install(
                &view_stack,
                &backend_nav,
                &diagnostics_nav,
                &overlay,
                myna_page,
                diagnostics_page,
            );
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
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(window),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
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
    let page = ready_page(controller, writer, rows, &overlay);
    overlay.set_child(Some(&page));
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
  </interface>\
</node>";

/// Drive the Myna page's shortcut row against a stand-in daemon on the session
/// bus, which the caller makes private.
fn shortcut_probe() -> glib::ExitCode {
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
    let registered = connection
        .register_object("/com/canonical/Myna/Dictation", &interface)
        .method_call({
            let shortcut = shortcut.clone();
            let asked = asked.clone();
            move |connection, _, path, interface, _, parameters, invocation| {
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
            move |_, _, _, _, _| shortcut.borrow().to_variant()
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
    let page = ready_page(controller, writer, rows, &overlay);
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
    let caps = || {
        let mut caps = Vec::new();
        let mut child = keys.first_child();
        while let Some(widget) = child {
            if widget.has_css_class("keycap") {
                if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                    caps.push(label.label().to_string());
                }
            }
            child = widget.next_sibling();
        }
        caps
    };

    if !settles(&|| button.is_sensitive()) {
        eprintln!("the shortcut row never offered to bind against a running daemon");
        return glib::ExitCode::FAILURE;
    }
    if button.label().as_deref() != Some("Set Up Shortcut") || keys.is_visible() {
        eprintln!(
            "an unbound daemon rendered {:?} with keys visible: {}",
            button.label(),
            keys.is_visible()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("shortcut-unbound: offered set-up");

    button.emit_clicked();
    if !settles(&|| !caps().is_empty()) {
        eprintln!("the granted shortcut never rendered as keys");
        return glib::ExitCode::FAILURE;
    }
    if caps() != ["Super", "J"] {
        eprintln!("expected Super+J key caps, got {:?}", caps());
        return glib::ExitCode::FAILURE;
    }
    if asked.borrow().as_deref() != Some("") {
        eprintln!(
            "set-up asked for {:?}, not the daemon's default",
            asked.borrow()
        );
        return glib::ExitCode::FAILURE;
    }
    if button.label().as_deref() != Some("Change Shortcut") {
        eprintln!("a bound shortcut offered {:?}", button.label());
        return glib::ExitCode::FAILURE;
    }
    println!("shortcut-bound: Super+J");
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
        }
    }

    fn applied(&self) -> Vec<Vec<String>> {
        self.applied.lock().expect("probe machine lock").clone()
    }

    fn snap(&self, arguments: &[&str]) -> Option<String> {
        let fixture = |text: &str| Some(text.to_owned());
        match arguments {
            ["list", "--unicode=never"] => fixture(
                "Name  Version  Rev  Tracking  Publisher  Notes\n\
                 myna  1.2.3  7  latest/stable  canonical**  -\n\
                 myna-parakeet  0.1.0  8  latest/stable  canonical**  -\n\
                 myna-whisper  0.1.0  9  latest/stable  canonical**  -\n",
            ),
            ["connections", "--all"] => {
                fixture(include_str!("../tests/fixtures/snap-connections.txt"))
            }
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

#[async_trait::async_trait(?Send)]
impl crate::ports::SystemConfigurator for ProbeMachine {
    async fn execute_privileged(
        &self,
        operations: &[crate::command::CommandRequest],
        _cancellation: crate::command::CancellationToken,
    ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure> {
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
        Ok(results)
    }
}

/// Drive the backend pages through the real repository adapter against a
/// fixture machine: discovery fills the sidebar, a backend page reads its
/// snapshot, an edit stages, and a confirmed apply is written and read back.
fn backends_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config backends probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }
    let application = adw::Application::builder()
        .application_id(probe_app_id())
        .build();
    let _ = application.register(None::<&gio::Cancellable>);

    let window = ui::MainWindow::new(&application);
    let view_stack = window.view_stack();
    let general_nav = window.general_nav();
    let backend_nav = window.backend_nav();
    let diagnostics_nav = window.diagnostics_nav();
    let overlay = window.overlay();
    let myna_page = match GioClientSettings::open() {
        Ok(settings) => build_myna_page(
            MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>),
            PersistenceWriter::spawn(GioClientSettings::open),
            &overlay,
        ),
        Err(error) => {
            eprintln!("myna-config backends probe could not open the settings store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    general_nav.replace(std::slice::from_ref(&myna_page));
    window.present();

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
        status_page("About and Diagnostics", "", "dialog-information-symbolic"),
    );

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

    // The Model tab always shows the single active backend, so the fixture's
    // active parakeet page should appear with no explicit selection.
    if !settles(&|| ui.controller().pages().len() == 2) {
        eprintln!(
            "discovery never listed the fixture backends ({} backends)",
            ui.controller().pages().len()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("backends-discovered: 2");

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
        eprintln!("the Parakeet page never showed the snapshot it read");
        return glib::ExitCode::FAILURE;
    }
    println!("backend-snapshot: read");

    idle_entry().expect("idle entry").set_text("600");
    let apply_button = || {
        content()
            .and_then(|page| {
                find_descendant(&page, &|widget| widget.is::<ui::BackendApplyControls>())
            })
            .and_then(|widget| widget.downcast::<ui::BackendApplyControls>().ok())
            .map(|controls| controls.apply_button())
    };
    if !settles(&|| apply_button().is_some_and(|button| button.is_sensitive())) {
        eprintln!("staging an edit never offered to apply it");
        return glib::ExitCode::FAILURE;
    }
    println!("backend-edit: staged");

    apply_button().expect("apply button").emit_clicked();
    let dialog = || {
        window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<adw::AlertDialog>().ok())
    };
    if !settles(&|| dialog().is_some()) {
        eprintln!("apply never asked for confirmation");
        return glib::ExitCode::FAILURE;
    }
    dialog()
        .expect("confirmation dialog")
        .emit_by_name::<()>("response", &[&"apply"]);
    let confirmed = || {
        content().is_some_and(|page| {
            find_descendant(&page, &|widget| {
                widget
                    .downcast_ref::<adw::PreferencesRow>()
                    .is_some_and(|row| row.title() == "Changes applied")
            })
            .is_some()
        })
    };
    if !settles(&confirmed) {
        eprintln!(
            "the apply was never confirmed by read-back; the machine ran {:?}",
            machine.applied()
        );
        return glib::ExitCode::FAILURE;
    }
    let wrote = machine.applied().iter().any(|operation| {
        operation
            .iter()
            .any(|argument| argument == "sleep-idle-seconds=600")
    });
    if !wrote || !idle_entry().is_some_and(|entry| entry.text() == "600") {
        eprintln!(
            "the apply did not write and show the staged value; the machine ran {:?}",
            machine.applied()
        );
        return glib::ExitCode::FAILURE;
    }
    println!("backend-apply: read back");

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

    ui.shutdown();
    window.close();
    glib::ExitCode::SUCCESS
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

fn build_myna_page(
    controller: Rc<MynaSettingsController>,
    writer: PersistenceWriter,
    overlay: &adw::ToastOverlay,
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
        PageState::Ready(rows) => ready_page(controller, writer, rows, overlay),
    }
}

fn status_page(title: &str, description: &str, icon: &str) -> adw::NavigationPage {
    let page = ui::StatusPage::new();
    page.set_status(title, description, icon);
    page.upcast()
}

fn error_page(detail: &str) -> adw::NavigationPage {
    status_page(
        &gettextrs::gettext("Myna Settings Unavailable"),
        detail,
        "dialog-error-symbolic",
    )
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

/// The schema description is hover-only: a permanent subtitle turns a
/// two-row page into a wall of prose.
fn describe(row: &impl IsA<gtk::Widget>, description: &str) {
    let row = row.as_ref();
    row.set_tooltip_text(Some(description));
    row.update_property(&[gtk::accessible::Property::Description(description)]);
}

fn ready_page(
    controller: Rc<MynaSettingsController>,
    writer: PersistenceWriter,
    rows: Vec<SettingRow>,
    overlay: &adw::ToastOverlay,
) -> adw::NavigationPage {
    let page = ui::MynaPage::new();
    crate::shortcut_ui::ShortcutControl::attach(
        page.shortcut_keys(),
        page.shortcut_button(),
        overlay.clone(),
        true,
        Box::new({
            let row = page.shortcut_row();
            move |state| row.set_subtitle(&crate::shortcut_ui::row_subtitle(state))
        }),
    );
    let group = page.settings_group();
    let bindings = Rc::new(RefCell::new(BTreeMap::<String, RowBinding>::new()));

    for setting in rows {
        let plan = widget_plan(setting.metadata());
        let reset = reset_button(&plan.key, &controller, &writer);
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
                row.add_suffix(&reset);
                if let Some(index) = setting
                    .value()
                    .as_str()
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
                group.add(&row);
            }
            WidgetKind::Number => {
                let (minimum, maximum) = plan.bounds.expect("Number plans carry bounds");
                let row = adw::SpinRow::with_range(minimum as f64, maximum as f64, 1.0);
                row.set_title(&plan.title);
                row.set_sensitive(plan.writable);
                row.set_value(setting.value().as_integer().unwrap_or(minimum) as f64);
                describe(&row, &plan.description);
                row.add_suffix(&reset);
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
                group.add(&row);
            }
            WidgetKind::Text => {
                let row = adw::EntryRow::builder()
                    .title(&plan.title)
                    .text(setting.value().as_str().unwrap_or_default())
                    .sensitive(plan.writable)
                    .show_apply_button(true)
                    .build();
                describe(&row, &plan.description);
                row.add_suffix(&reset);
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
                group.add(&row);
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
                                controller
                                    .row(&key)
                                    .expect("event refers to an existing row")
                                    .value(),
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

fn reset_button(
    key: &str,
    controller: &Rc<MynaSettingsController>,
    writer: &PersistenceWriter,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .tooltip_text(gettextrs::gettext("Reset to the schema default"))
        .valign(gtk::Align::Center)
        .build();
    let accessible_label = gettextrs::gettext("Reset to the schema default");
    button.update_property(&[gtk::accessible::Property::Label(&accessible_label)]);
    button.connect_clicked({
        let key = key.to_owned();
        let controller = controller.clone();
        let writer = writer.clone();
        move |_| {
            if let Ok(request) = controller.reset(&key) {
                persist_request(writer.clone(), controller.clone(), request, None);
            }
        }
    });
    button
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
