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
use crate::ports::{ClientSettings, ClientSettingsError};
use crate::ui;
use crate::APP_ID;

pub use crate::myna_settings::{widget_plan, WidgetKind, WidgetPlan};

const SMOKE_ENV: &str = "MYNA_CONFIG_SMOKE_BUILD";
const TEMPLATE_ENV: &str = "MYNA_CONFIG_TEMPLATE_TEST";
const ACCESSIBILITY_ENV: &str = "MYNA_CONFIG_ACCESSIBILITY_TEST";

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

    let application = adw::Application::builder().application_id(APP_ID).build();
    let _ = application.register(None::<&gio::Cancellable>);

    let window = ui::MainWindow::new(&application);
    let split_view = window.split_view();

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

    split_view.set_content(Some(&diagnostics));
    add_narrow_breakpoint(&window, &split_view);
    install_appearance_policy(&window);
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
    if !split_view.is_collapsed() {
        eprintln!("diagnostics layout did not collapse below the 600sp breakpoint");
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

fn template_probe() -> glib::ExitCode {
    ui::register_resources();
    if let Err(error) = gtk::init() {
        eprintln!("myna-config template probe could not initialize GTK: {error}");
        return glib::ExitCode::FAILURE;
    }

    let application = adw::Application::builder().application_id(APP_ID).build();
    let _ = application.register(None::<&gio::Cancellable>);
    for resource in [
        "active-backend-dialog.ui",
        "apply-dialog.ui",
        "backend-apply-controls.ui",
        "backend-page.ui",
        "diagnostics-page.ui",
        "main-window.ui",
        "myna-page.ui",
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
        window.split_view(),
        window.sidebar_list(),
        window.myna_row(),
        window.diagnostics_row(),
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
    );
    println!("MynaPage");
    let backend = ui::BackendPage::new();
    let _ = (backend.preferences_page(), backend.refresh_button());
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
    let sidebar = ui::SidebarRow::new();
    let _ = sidebar.icon();
    println!("SidebarRow");
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

fn build_window(application: &adw::Application) {
    ui::register_resources();
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }

    gtk::Window::set_default_icon_name(APP_ID);
    let window = ui::MainWindow::new(application);
    let split_view = window.split_view();
    let sidebar_list = window.sidebar_list();
    let overlay = window.overlay();
    let myna_row = window.myna_row().upcast::<gtk::ListBoxRow>();
    let diagnostics_row = window.diagnostics_row().upcast::<gtk::ListBoxRow>();

    split_view.set_content(Some(&status_page(
        &gettextrs::gettext("Loading Myna Settings"),
        &gettextrs::gettext("Reading the installed settings schema…"),
        "content-loading-symbolic",
    )));
    sidebar_list.select_row(Some(&myna_row));
    add_narrow_breakpoint(&window, &split_view);
    install_appearance_policy(&window);
    window.present();

    glib::idle_add_local_once(glib::clone!(
        #[weak]
        split_view,
        #[weak]
        overlay,
        #[weak]
        sidebar_list,
        #[strong]
        myna_row,
        #[strong]
        diagnostics_row,
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
            split_view.set_content(Some(&myna_page));

            let diagnostics_page = status_page(
                &gettextrs::gettext("About and Diagnostics"),
                &gettextrs::gettext("Backend diagnostics will appear after discovery."),
                "dialog-information-symbolic",
            );

            let ui = crate::backend_ui::BackendUi::install(
                &split_view,
                &overlay,
                myna_row.clone(),
                myna_page,
                diagnostics_row.clone(),
                diagnostics_page,
                sidebar_list.clone(),
            );
            window.connect_close_request(move |_| {
                ui.shutdown();
                glib::Propagation::Proceed
            });
        }
    ));
}

fn add_narrow_breakpoint(window: &ui::MainWindow, split_view: &adw::NavigationSplitView) {
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        600.0,
        adw::LengthUnit::Sp,
    ));
    narrow.add_setter(split_view, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);
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

fn apply_appearance_policy(window: &ui::MainWindow) {
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

fn install_appearance_policy(window: &ui::MainWindow) {
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
    fn apply(&self, value: &ClientSettingValue, pending: bool) {
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
                row.set_sensitive(*writable && !pending);
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
                row.set_text(value.as_str().unwrap_or_default());
                row.set_sensitive(*writable && !pending);
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
