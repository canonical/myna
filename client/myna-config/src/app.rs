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

const ICON_RESOURCES: &str = "/com/canonical/Myna/Config/icons";
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
    ui::register_resources();
    // libadwaita replaces the GTK theme name as it starts; keep the desktop's.
    if gtk::init().is_ok() {
        crate::platform::appearance::remember_startup_theme();
    }
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

/// Read the machine once, then open either the onboarding wizard or the
/// settings window. The read is the same two snapd reads a startup refresh
/// already budgets for (`/v2/snaps`, `/v2/connections`), and the wizard is
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
            &crate::adapters::snap_backend::SnapBackendRepository::new(
                runner.clone(),
                std::sync::Arc::new(crate::adapters::snapd_client::UnixSocketSnapdClient::new()),
            ),
            &crate::adapters::system_configurator::PkexecSystemConfigurator::new(runner),
            crate::platform::Platform::current().components().as_ref(),
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
    let processing = processing_group();
    window
        .processing_nav()
        .replace(&[processing_page(&processing, &window.processing_nav())]);
    watch_chain_status(&processing.notice, daemon_chain_status);
    let steps = attach_steps(&processing, &overlay, &window.processing_nav());
    let models = models_section(&overlay, &view_stack, steps);

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
        #[strong]
        processing,
        #[strong]
        models,
        move || {
            let (myna_page, general_settings) = match GioClientSettings::open() {
                Ok(settings) => {
                    let writer = PersistenceWriter::spawn(GioClientSettings::open);
                    let controller =
                        MynaSettingsController::load(Rc::new(settings) as Rc<dyn ClientSettings>);
                    let spoken_language = crate::backend_ui::spoken_language_group();
                    let page = build_myna_page(
                        controller.clone(),
                        writer,
                        &overlay,
                        &spoken_language,
                        &processing,
                    );
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
            ui.attach_models(models);
            ui.install_window_actions(&window);
            window.connect_close_request(move |_| {
                ui.shutdown();
                glib::Propagation::Proceed
            });
        }
    ));
}

fn current_appearance_policy() -> AppearancePolicy {
    let readings = crate::platform::Platform::current().appearance().read();
    appearance_policy(!readings.reduced_motion, readings.high_contrast)
}

fn apply_appearance_policy(window: &gtk::Widget) {
    let policy = current_appearance_policy();
    adw::StyleManager::default()
        .set_color_scheme(crate::platform::Platform::current().color_scheme());
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
    let subscription = crate::platform::Platform::current()
        .appearance()
        .watch(Box::new(glib::clone!(
            #[weak]
            window,
            move |_| apply_appearance_policy(&window)
        )));
    let subscription = std::cell::RefCell::new(Some(subscription));
    window.connect_destroy(move |_| drop(subscription.borrow_mut().take()));
}

/// Rows placed with the active model go into `model_settings`, which the
/// Model tab shows, and processing rows into `processing`, which its own tab
/// shows.
fn build_myna_page(
    controller: Rc<MynaSettingsController>,
    writer: PersistenceWriter,
    overlay: &adw::ToastOverlay,
    model_settings: &adw::PreferencesGroup,
    processing: &ProcessingGroups,
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
        PageState::Ready(rows) => ready_page(
            controller,
            writer,
            rows,
            overlay,
            model_settings,
            processing,
        ),
    }
}

/// The groups the Processing tab shows; General's builder adds their rows.
#[derive(Clone)]
struct ProcessingGroups {
    /// Advice when the chosen chain has no model for a step; hidden otherwise.
    notice: adw::Banner,
    /// The saved chains, one to pick.
    chain: adw::PreferencesGroup,
    /// Get more steps.
    more: adw::PreferencesGroup,
    /// The Past dictations sub-page: the record switch and the list.
    past: crate::past_dictations_ui::PastPage,
    /// The link to it, at the bottom of the page.
    past_link: adw::PreferencesGroup,
}

fn processing_group() -> ProcessingGroups {
    ProcessingGroups {
        notice: adw::Banner::builder().use_markup(false).build(),
        more: adw::PreferencesGroup::new(),
        chain: adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Chains"))
            .description(gettextrs::gettext(
                "What Myna does to what you say. Turn one chain on, or open it to change its steps. While a chain is on, its text shows on screen as you speak and is typed a sentence at a time. Experimental.",
            ))
            .build(),
        past: crate::past_dictations_ui::PastPage::new(),
        past_link: adw::PreferencesGroup::new(),
    }
}

/// Give the Steps group its rows, over the real store, daemon and snapd.
fn attach_steps(
    groups: &ProcessingGroups,
    overlay: &adw::ToastOverlay,
    nav: &adw::NavigationView,
) -> Option<Rc<crate::processing_ui::StepsPage>> {
    let store = match GioClientSettings::open() {
        Ok(store) => Rc::new(store),
        Err(error) => {
            glib::g_warning!(crate::LOG_DOMAIN, "steps unavailable: {error}");
            return None;
        }
    };
    let deps = crate::processing_ui::Deps {
        store,
        daemon: Rc::new(crate::adapters::dictation_daemon::SessionBusDaemon::default()),
        configurator: Rc::new(
            crate::adapters::system_configurator::PkexecSystemConfigurator::new(
                std::sync::Arc::new(crate::command::GioCommandRunner),
            ),
        ),
        repository: Rc::new(crate::adapters::snap_backend::SnapBackendRepository::new(
            std::sync::Arc::new(crate::command::GioCommandRunner),
            std::sync::Arc::new(crate::adapters::snapd_client::UnixSocketSnapdClient::new()),
        )),
        catalog: Box::new(|| {
            crate::plugin_catalog::load_listing(
                std::path::Path::new(crate::plugin_catalog::SNAPSHOT_DIR),
                std::path::Path::new("/snap"),
                crate::plugin_catalog::Listing::from_env(
                    std::env::var("MYNA_LIST_UNCURATED").ok().as_deref(),
                ),
            )
            .entries
        }),
        locales: myna_core::locale::preferred_languages(&myna_core::locale::SystemLocale::default()),
        overlay: overlay.clone(),
        nav: nav.clone(),
        follow_interval: Duration::from_secs(1),
    };
    Some(crate::processing_ui::StepsPage::attach(
        deps,
        &crate::processing_ui::Slots {
            chains: groups.chain.clone(),
            more: groups.more.clone(),
        },
    ))
}

/// The Models groups of Diagnostics. Add text model opens Processing's
/// catalog, the one place text models install from.
fn models_section(
    overlay: &adw::ToastOverlay,
    view_stack: &adw::ViewStack,
    steps: Option<Rc<crate::processing_ui::StepsPage>>,
) -> Rc<crate::models_ui::ModelsSection> {
    let section = crate::models_ui::ModelsSection::new(crate::models_ui::Deps {
        configurator: Rc::new(
            crate::adapters::system_configurator::PkexecSystemConfigurator::new(
                std::sync::Arc::new(crate::command::GioCommandRunner),
            ),
        ),
        daemon: Rc::new(crate::adapters::dictation_daemon::SessionBusDaemon::default()),
        overlay: overlay.clone(),
        follow_interval: Duration::from_secs(1),
        catalog: Box::new(|| {
            crate::plugin_catalog::load_listing(
                std::path::Path::new(crate::plugin_catalog::SNAPSHOT_DIR),
                std::path::Path::new("/snap"),
                crate::plugin_catalog::Listing::from_env(
                    std::env::var("MYNA_LIST_UNCURATED").ok().as_deref(),
                ),
            )
            .entries
        }),
    });
    match steps {
        Some(steps) => section.connect_add_text({
            let view_stack = view_stack.downgrade();
            move || {
                if let Some(view_stack) = view_stack.upgrade() {
                    view_stack.set_visible_child_name("processing");
                }
                steps.open_catalog_to_add();
            }
        }),
        None => section.add_text_unavailable(),
    }
    section
}

fn processing_page(groups: &ProcessingGroups, nav: &adw::NavigationView) -> adw::NavigationPage {
    let preferences = adw::PreferencesPage::new();
    preferences.set_vexpand(true);
    preferences.add(&groups.chain);
    preferences.add(&groups.more);
    groups.past_link.add(&groups.past.link_row(nav));
    preferences.add(&groups.past_link);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&groups.notice);
    content.append(&preferences);
    adw::NavigationPage::builder()
        .title(gettextrs::gettext("Processing"))
        .child(&content)
        .build()
}

/// Show the advice for `state`, or hide the banner when there is none.
fn show_chain_notice(banner: &adw::Banner, state: Option<&crate::chain_notice::ChainState>) {
    match state.and_then(crate::chain_notice::ChainState::notice) {
        Some(text) => {
            banner.set_title(&text);
            banner.set_revealed(true);
        }
        None => banner.set_revealed(false),
    }
}

/// Follow the daemon's chain status for as long as the banner lives. The
/// daemon rebinds whenever a model is connected or removed, so a change
/// clears or raises the notice without reopening Settings; the daemon
/// publishes it, so there is nothing to poll.
fn watch_chain_status(
    banner: &adw::Banner,
    read: impl Fn() -> Option<crate::chain_notice::ChainState> + Clone + Send + 'static,
) {
    let shown = banner.downgrade();
    let update: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
        let Some(banner) = shown.upgrade() else {
            return;
        };
        let read = read.clone();
        glib::spawn_future_local(async move {
            let state = gio::spawn_blocking(read).await.ok().flatten();
            show_chain_notice(&banner, state.as_ref());
        });
    });
    update();
    let again = update.clone();
    crate::machine::watch_daemon(move || again());
}

/// The daemon's chain status, `None` when it is not running or too old.
fn daemon_chain_status() -> Option<crate::chain_notice::ChainState> {
    crate::machine::daemon_report().and_then(|report| report.chain)
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
    List(Rc<crate::list_editor_ui::ListEditor>),
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
            Self::List(editor) => editor.apply(value.as_list().unwrap_or_default()),
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
    processing: &ProcessingGroups,
) -> adw::NavigationPage {
    let page = ui::MynaPage::new();
    crate::shortcut_ui::ShortcutControl::attach(
        page.shortcut_keys(),
        page.shortcut_button(),
        overlay.clone(),
        crate::shortcut_ui::Surface::Row,
        page.in_place(),
        Box::new({
            // The row holds the button, which owns the control.
            let row = page.shortcut_row().downgrade();
            move |state, refusal| {
                if let Some(row) = row.upgrade() {
                    let subtitle = refusal
                        .map_or_else(|| crate::shortcut_ui::row_subtitle(state), str::to_owned);
                    row.set_subtitle(&crate::markup::escape_markup(&subtitle));
                    row.set_subtitle_selectable(
                        *state == crate::shortcut::ShortcutState::Unsupported,
                    );
                }
            }
        }),
    );
    let dictation = page.settings_group();
    let bindings = Rc::new(RefCell::new(BTreeMap::<String, RowBinding>::new()));

    for setting in rows {
        let plan = widget_plan(setting.metadata());
        let group = match plan.placement {
            Placement::Dictation => &dictation,
            Placement::ActiveModel => model_settings,
            Placement::ProcessingRecord => &processing.past.record,
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
            WidgetKind::List => {
                let editor = crate::list_editor_ui::ListEditor::new(
                    group.clone(),
                    &plan,
                    setting.value().as_list().unwrap_or_default(),
                    overlay.clone(),
                    Box::new({
                        let controller = controller.clone();
                        let key = plan.key.clone();
                        let writer = writer.clone();
                        move |values| {
                            if let Ok(request) =
                                controller.set(&key, ClientSettingValue::List(values))
                            {
                                persist_request(writer.clone(), controller.clone(), request, None);
                            }
                        }
                    }),
                );
                bindings
                    .borrow_mut()
                    .insert(plan.key.clone(), RowBinding::List(editor));
            }
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
                if plan.key == myna_core::settings::KEY_PROCESSING_HISTORY {
                    processing.past.follow_record(&row);
                }
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
    attach_sound_preview(&bindings.borrow(), overlay);
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

/// The Sound style row's Preview button: the daemon plays the selected set,
/// start, stop and error. Insensitive while the Sounds switch is off, as a
/// session would be silent too.
fn attach_sound_preview(bindings: &BTreeMap<String, RowBinding>, overlay: &adw::ToastOverlay) {
    let Some(RowBinding::Choice { row, choices, .. }) =
        bindings.get(myna_core::settings::KEY_SOUND_SET)
    else {
        return;
    };
    let button = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text(gettextrs::gettext("Preview"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
        "Preview sound style",
    ))]);
    if let Some(RowBinding::Switch { row: sounds, .. }) =
        bindings.get(myna_core::settings::KEY_SOUNDS)
    {
        sounds
            .bind_property("active", &button, "sensitive")
            .sync_create()
            .build();
    }
    button.connect_clicked({
        let row = row.downgrade();
        let choices = choices.clone();
        let overlay = overlay.downgrade();
        move |_| {
            let Some(set) = row
                .upgrade()
                .and_then(|row| choices.get(row.selected() as usize).cloned())
            else {
                return;
            };
            let overlay = overlay.clone();
            crate::sound_preview::request(&set, move |refusal| {
                let Some(refusal) = refusal else {
                    return;
                };
                if let crate::sound_preview::PreviewRefusal::Failed(detail) = &refusal {
                    glib::g_warning!(crate::LOG_DOMAIN, "sound preview failed: {detail}");
                }
                if let Some(overlay) = overlay.upgrade() {
                    overlay.add_toast(adw::Toast::new(&refusal.message()));
                }
            });
        }
    });
    row.add_suffix(&button);
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
