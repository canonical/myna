//! GTK/libadwaita wiring for backend pages and the top-level tab bar.
//!
//! This module glues [`BackendController`] to the widgets. It is deliberately
//! thin: no domain decisions live here — the controller decides which pages
//! exist and what data they carry, and this module renders that view.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gtk::{gio, glib};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::active_backend::{
    execute_switch, ActiveBackendController, PrepareSwitchError, SwitchNotice, SwitchOutcome,
};
use crate::adapters::snap_backend::SnapBackendRepository;
use crate::adapters::system_configurator::PkexecSystemConfigurator;
use crate::backend_apply::{
    execute_backend_apply, prepare_change, ApplyFailure, ApplyPreview, ApplySuccess,
    PrepareApplyError, ValidationIssue,
};
use crate::backend_controller::{
    BackendController, BackendPage, ConnectionKind, ControllerEvent, DiscoveryRequest,
};
use crate::command::{CancellationToken, GioCommandRunner};
use crate::diagnostics::{
    self, present_diagnostics, BackendDiagnostic, DiagnosticConnection, DiagnosticInput,
    InstalledSnap, OnboardingState, REFRESH_DEBOUNCE,
};
use crate::domain::{ActiveBackendState, BackendIdentity, ConfigValue, ServiceState};
use crate::markup::escape_markup;
use crate::model_family::{
    better_model_hint, coverage, coverage_cell, coverage_columns, coverage_count, coverage_summary,
    installable_families, installing_hint, is_recommended, model_family, recommendation,
    recommendation_label, recommended_first, Coverage, Named, Recommendation,
};
use crate::onboarding::{family_offer, ModelOffer};
use crate::operation_gate::{OperationCoordinator, OperationKind};
use crate::performance::PerformanceFacts;
use crate::ports::{BackendRepository, SystemConfigurator};
use crate::presentation::{ControlType, PresentationRow, Sensitivity};
use crate::snap_changes::ApplyProgress;
use crate::ui;

struct ModelRow {
    backend: BackendIdentity,
    row: adw::ActionRow,
    /// None when the row is the only model and already connected.
    radio: Option<gtk::CheckButton>,
    pill: gtk::Label,
    /// None for a backend of no known family.
    languages: Option<gtk::MenuButton>,
    spinner: gtk::Spinner,
}

impl ModelRow {
    /// What a screen reader lands on: the radio, or the row when there is none.
    fn focus_target(&self) -> gtk::Widget {
        self.radio
            .clone()
            .map(Cast::upcast)
            .unwrap_or_else(|| self.row.clone().upcast())
    }
}

/// A model row's accessible description: its subtitle, then the pill's text
/// when it is recommended. libadwaita describes the row by its subtitle
/// label, which GTK 4.14 reads out as nothing, so the rows say it instead.
/// Indents each line's wrapped continuation to `columns[line]` characters of
/// the view's monospace font, so a long value wraps under itself.
fn hang_continuations(view: &gtk::TextView, columns: &[usize]) {
    let buffer = view.buffer();
    let table = buffer.tag_table();
    for (line, &column) in columns.iter().enumerate() {
        if column == 0 {
            continue;
        }
        let name = format!("hang-{column}");
        let tag = table.lookup(&name).unwrap_or_else(|| {
            let width = view
                .create_pango_layout(Some(&" ".repeat(column)))
                .pixel_size()
                .0;
            // Pango's negative indent keeps the first line at the margin
            // and moves the others in by its size.
            let tag = gtk::TextTag::builder()
                .name(name.as_str())
                .indent(-width)
                .build();
            table.add(&tag);
            tag
        });
        let Ok(line) = i32::try_from(line) else {
            break;
        };
        let (Some(start), Some(mut end)) = (buffer.iter_at_line(line), buffer.iter_at_line(line))
        else {
            continue;
        };
        end.forward_to_line_end();
        buffer.apply_tag(&tag, &start, &end);
    }
}

fn model_description(subtitle: Option<&str>, pill: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = [subtitle, pill]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn recommended_pill() -> gtk::Label {
    gtk::Label::builder()
        .label(gettextrs::gettext("Recommended"))
        .valign(gtk::Align::Center)
        .css_classes(["recommended-pill"])
        .build()
}

/// Shows `named` on `label`, each language name shaped as that language.
fn set_named(label: &gtk::Label, named: &Named) {
    label.set_label(&named.text);
    let attributes = gtk::pango::AttrList::new();
    for (range, endonym) in &named.names {
        let Some(code) = endonym.pango_language() else {
            continue;
        };
        let mut language = gtk::pango::AttrLanguage::new(&gtk::pango::Language::from_string(code));
        language.set_start_index(range.start as u32);
        language.set_end_index(range.end as u32);
        attributes.insert(language);
    }
    label.set_attributes(Some(&attributes));
}

/// `named` as Pango markup, each language name tagged with its language.
fn named_markup(named: &Named) -> String {
    let escape = |text: &str| glib::markup_escape_text(text).to_string();
    let mut markup = String::new();
    let mut at = 0;
    for (range, endonym) in &named.names {
        markup.push_str(&escape(&named.text[at..range.start]));
        let name = escape(&named.text[range.clone()]);
        match endonym.pango_language() {
            Some(code) => markup.push_str(&format!("<span lang=\"{code}\">{name}</span>")),
            None => markup.push_str(&name),
        }
        at = range.end;
    }
    markup.push_str(&escape(&named.text[at..]));
    markup
}

/// A flat info button that opens `family`'s full language list, built
/// afresh each time for the language `user_language` then returns. Its
/// summary is the tooltip and accessible name. Its own image child keeps
/// GTK from drawing a dropdown arrow.
fn languages_button(
    family: myna_core::language::ModelFamily,
    user_language: impl Fn() -> Option<String> + 'static,
) -> gtk::MenuButton {
    let icon =
        gio::ThemedIcon::from_names(&["info-outline-symbolic", "dialog-information-symbolic"]);
    let button = gtk::MenuButton::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .child(&gtk::Image::from_gicon(&icon))
        .build();
    button.set_create_popup_func(move |button| {
        let coverage = coverage(family, user_language().as_deref());
        button.set_popover(Some(&languages_popover(&coverage)));
    });
    button.update_property(
        &[gtk::accessible::Property::Description(&gettextrs::gettext(
            "Languages",
        ))],
    );
    show_languages(&button, family, None);
    button
}

fn show_languages(
    button: &gtk::MenuButton,
    family: myna_core::language::ModelFamily,
    user_language: Option<&str>,
) {
    let summary = coverage_summary(&coverage(family, user_language));
    button.set_tooltip_markup(Some(&named_markup(&summary)));
    // Otherwise the name takes in the open popover's text too.
    button.update_property(&[gtk::accessible::Property::Label(&summary.text)]);
}

fn languages_popover(coverage: &Coverage) -> gtk::Popover {
    let count = coverage.languages.len();
    let columns = coverage_columns(count);
    let names = gtk::Grid::builder()
        .css_classes(["language-columns"])
        .row_homogeneous(true)
        .row_spacing(6)
        .column_spacing(24)
        .build();
    for (index, endonym) in coverage.languages.iter().enumerate() {
        let label = gtk::Label::builder().xalign(0.0).build();
        set_named(&label, &Named::of(*endonym));
        if index == 0 && coverage.user_first {
            label.add_css_class("heading");
            label.add_css_class("accent");
            label.update_property(
                &[gtk::accessible::Property::Description(&gettextrs::gettext(
                    "Your language",
                ))],
            );
        }
        let (column, row) = coverage_cell(index, count, columns);
        names.attach(&label, column as i32, row as i32, 1, 1);
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(coverage_count(coverage))
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    content.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .child(&names)
            .build(),
    );
    gtk::Popover::builder().child(&content).build()
}

type InstallFamily = Rc<dyn Fn(myna_core::language::ModelFamily)>;

/// What the Install more models dialog lists: the families, the recommended
/// one and the language it is recommended for.
#[derive(Clone, PartialEq)]
struct InstallOffer {
    families: Vec<myna_core::language::ModelFamily>,
    recommended: Option<myna_core::language::ModelFamily>,
    user_language: Option<String>,
}

/// An offered family's row and its install control.
struct OfferedRow {
    family: myna_core::language::ModelFamily,
    row: adw::ActionRow,
    control: ui::InstallControl,
}

/// The Install more models dialog while it is open.
struct OpenInstallDialog {
    dialog: glib::WeakRef<ui::InstallModelsDialog>,
    /// What its rows show.
    shown: InstallOffer,
    rows: Vec<OfferedRow>,
}

/// Where installing a family from Install more models stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelInstall {
    /// snapd is installing it: its polkit prompt may still be open, and the
    /// percentage appears once a download announces its size.
    Running(Option<u8>),
    /// snapd is done; the row waits for a discovery that shows it.
    Confirming,
}

/// How often an install's change is read for its progress.
const FOLLOW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

fn glib_sleep(
    interval: std::time::Duration,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
    Box::pin(glib::timeout_future(interval))
}

/// One row per family, `recommended` with its pill, each with its download
/// size and an install control whose button hands its family to `install`.
fn offer_install(
    dialog: &ui::InstallModelsDialog,
    offer: &InstallOffer,
    nvidia_gpu: bool,
    install: InstallFamily,
) -> Vec<OfferedRow> {
    let InstallOffer {
        families,
        recommended,
        user_language,
    } = offer;
    let recommended = *recommended;
    let mut offered = Vec::new();
    for &family in families {
        let shown = model_family(family.snap_name());
        let row = adw::ActionRow::builder()
            .title(&shown.name)
            .use_markup(false)
            .build();
        let subtitle = offer_subtitle(
            shown.description.as_deref(),
            family_offer(family, nvidia_gpu),
        );
        row.set_subtitle(&subtitle);
        let pill = (recommended == Some(family)).then(|| {
            let pill = recommended_pill();
            set_named(
                &pill,
                &recommendation_label(family, user_language.as_deref()),
            );
            pill
        });
        if let Some(pill) = &pill {
            row.add_suffix(pill);
        }
        let languages = languages_button(family, {
            let user_language = user_language.clone();
            move || user_language.clone()
        });
        show_languages(&languages, family, user_language.as_deref());
        row.add_suffix(&languages);
        let control = ui::InstallControl::new();
        row.add_suffix(&control);
        control.button().connect_clicked({
            let install = Rc::clone(&install);
            move |_| install(family)
        });
        row.reset_relation(gtk::AccessibleRelation::DescribedBy);
        let pill_text = pill.as_ref().map(gtk::Label::label);
        if let Some(description) = model_description(Some(&subtitle), pill_text.as_deref()) {
            row.update_property(&[gtk::accessible::Property::Description(&description)]);
        }
        offered.push(OfferedRow {
            family,
            row,
            control,
        });
    }
    dialog.replace_rows(offered.iter().map(|offered| offered.row.clone()).collect());
    offered
}

/// An offered family's subtitle: what it is good at, then what installing it
/// downloads.
fn offer_subtitle(description: Option<&str>, offer: ModelOffer) -> String {
    let size = crate::onboarding_ui::download_size(offer.download_bytes);
    let size = if offer.upper_bound {
        // TRANSLATORS: an upper bound on a download size, such as "up to 1.4 GB": it is less on a machine whose GPU cannot be used.
        gettextrs::gettext("up to {size}").replace("{size}", &size)
    } else {
        size
    };
    match description {
        // TRANSLATORS: {description} says what a model is good at, such as "Most languages, with uneven accuracy", and {size} what installing it downloads, such as "198 MB" or "up to 1.4 GB". The spaces around the dot are no-break spaces, so the size never starts a line of its own.
        Some(description) => gettextrs::gettext("{description}\u{a0}·\u{a0}{size}")
            .replace("{description}", description)
            .replace("{size}", &size),
        None => size,
    }
}

/// The families the open dialog lists: the ones it opened with, since an
/// installed one stays to show it is, and any missing since, the
/// recommended one first.
fn listed_families(
    shown: &[myna_core::language::ModelFamily],
    missing: &[myna_core::language::ModelFamily],
    recommended: Option<myna_core::language::ModelFamily>,
) -> Vec<myna_core::language::ModelFamily> {
    let (first, rest): (Vec<_>, Vec<_>) = myna_core::language::ModelFamily::ALL
        .into_iter()
        .filter(|family| shown.contains(family) || missing.contains(family))
        .partition(|family| Some(*family) == recommended);
    first.into_iter().chain(rest).collect()
}

/// Runtime coordinator that keeps the Backend/Diagnostics tabs in sync with a
/// [`BackendController`]. The Model tab always shows the single active
/// backend; there is no chooser or per-backend list to maintain.
pub struct BackendUi {
    controller: Rc<BackendController>,
    configurator: Rc<dyn SystemConfigurator>,
    view_stack: adw::ViewStack,
    backend_nav: adw::NavigationView,
    diagnostics_nav: adw::NavigationView,
    overlay: adw::ToastOverlay,
    myna_selector: Option<ui::MynaPage>,
    /// The spoken-language row's group, built with General's rows and shown
    /// on the Model tab while the active backend takes it.
    spoken_language: Option<adw::PreferencesGroup>,
    /// General's settings, told what the active backend does by default so
    /// "When to transcribe" can show it.
    client_settings: Option<Rc<crate::myna_settings::MynaSettingsController>>,
    /// The General tab's model rows and the backend each one chooses.
    model_rows: RefCell<Vec<ModelRow>>,
    /// Never shown. GTK draws a check button as a radio only while it is in a
    /// group, so a lone model's radio needs this partner to look like one.
    model_radio_group: gtk::CheckButton,
    /// Set while rendering marks a radio, so the mark is not taken as a choice.
    marking_models: std::cell::Cell<bool>,
    /// The user's languages, most preferred first, once read off the main
    /// thread; no model is recommended until then.
    preferred_languages: RefCell<Option<Vec<String>>>,
    this: std::rc::Weak<Self>,
    operation_coordinator: OperationCoordinator,
    active_backend: ActiveBackendController,
    diagnostics_page: RefCell<Option<adw::NavigationPage>>,
    backend_pages: RefCell<BTreeMap<String, ui::BackendPage>>,
    apply_state: RefCell<BTreeMap<String, PendingChange>>,
    /// The active-backend state the Model tab last rendered.
    shown_state: RefCell<Option<ActiveBackendState>>,
    backend_tab_shown: std::cell::Cell<bool>,
    installed_snaps: RefCell<Vec<InstalledSnap>>,
    inventory_complete: std::cell::Cell<bool>,
    inventory_failure: RefCell<Option<String>>,
    /// The last clock probe and pressure reading. Refreshed with every
    /// discovery, off the main thread, so the page never spins a core itself.
    performance: RefCell<Option<PerformanceFacts>>,
    last_diagnostics_refresh: std::cell::Cell<Option<Instant>>,
    /// The Install more models dialog while it is open.
    install_dialog: RefCell<Option<OpenInstallDialog>>,
    /// Families being installed from it, which outlive the dialog.
    model_installs:
        RefCell<std::collections::HashMap<myna_core::language::ModelFamily, ModelInstall>>,
    /// Stop following installs when the window closes; snapd carries on.
    install_cancellation: CancellationToken,
    follow_interval: std::cell::Cell<std::time::Duration>,
    /// Sizes each family's download for the engine its install hook picks.
    nvidia_gpu: bool,
}

#[derive(Clone, Copy)]
enum DiagnosticsFocus {
    Copy,
    Refresh,
    Report,
}

struct BackendFocus {
    widget_name: glib::GString,
    entry: Option<EntryFocus>,
}

/// What a rebuild must hand back to the entry row the user is typing in.
struct EntryFocus {
    text: glib::GString,
    cursor_position: i32,
}

/// The one Model tab change being applied, from the user's change until the
/// read-back: every change applies on its own.
struct PendingChange {
    key: String,
    /// Shown on the row while it applies.
    value: ConfigValue,
    operation_token: u64,
    cancellation: CancellationToken,
    /// The apply's own state.
    progress_message: String,
    /// What snapd is doing for it, as last polled.
    progress_detail: Option<String>,
    /// The row to focus once the change is done, since the page is rebuilt.
    focus: Option<BackendFocus>,
}

impl PendingChange {
    /// The line the changed row shows: what snapd is doing, else the
    /// apply's own state.
    fn progress(&self) -> &str {
        self.progress_detail
            .as_deref()
            .unwrap_or(&self.progress_message)
    }
}

/// What a page rebuild shows of the change being applied.
#[derive(Clone, Debug)]
struct ApplyingView {
    key: String,
    value: ConfigValue,
    progress: String,
}

/// Signals a vanished backend's apply to stop. The entry, and with it the
/// operation gate, stays until the apply returns.
fn cancel_apply_state(
    state: &mut BTreeMap<String, PendingChange>,
    coordinator: &OperationCoordinator,
    snap_name: &str,
) {
    if let Some(change) = state.get(snap_name) {
        change.cancellation.cancel();
        coordinator.cancel(change.operation_token);
    }
}

fn abandon_all_apply_state(
    state: &mut BTreeMap<String, PendingChange>,
    coordinator: &OperationCoordinator,
) {
    for (_, change) in std::mem::take(state) {
        coordinator.abandon(change.operation_token);
        change.cancellation.cancel();
    }
}

/// General's GSettings rows that follow the connected backend.
pub struct GeneralSettings {
    pub spoken_language: adw::PreferencesGroup,
    pub controller: Rc<crate::myna_settings::MynaSettingsController>,
}

impl BackendUi {
    pub fn install(
        view_stack: &adw::ViewStack,
        backend_nav: &adw::NavigationView,
        diagnostics_nav: &adw::NavigationView,
        overlay: &adw::ToastOverlay,
        myna_page: adw::NavigationPage,
        general_settings: Option<GeneralSettings>,
        diagnostics_page: adw::NavigationPage,
    ) -> Rc<Self> {
        let repository: Rc<dyn BackendRepository> =
            Rc::new(SnapBackendRepository::new(Arc::new(GioCommandRunner)));
        let configurator: Rc<dyn SystemConfigurator> =
            Rc::new(PkexecSystemConfigurator::new(Arc::new(GioCommandRunner)));
        let ui = Self::install_with_ports(
            repository,
            configurator,
            view_stack,
            backend_nav,
            diagnostics_nav,
            overlay,
            myna_page,
            general_settings,
            diagnostics_page,
        );
        ui.read_preferred_languages(|| {
            myna_core::locale::preferred_languages(&myna_core::locale::SystemLocale::default())
        });
        ui
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn install_with_ports(
        repository: Rc<dyn BackendRepository>,
        configurator: Rc<dyn SystemConfigurator>,
        view_stack: &adw::ViewStack,
        backend_nav: &adw::NavigationView,
        diagnostics_nav: &adw::NavigationView,
        overlay: &adw::ToastOverlay,
        myna_page: adw::NavigationPage,
        general_settings: Option<GeneralSettings>,
        diagnostics_page: adw::NavigationPage,
    ) -> Rc<Self> {
        let controller = BackendController::new(repository);
        let (spoken_language, client_settings) = general_settings
            .map(|settings| (settings.spoken_language, settings.controller))
            .unzip();
        let myna_selector = myna_page.downcast::<ui::MynaPage>().ok();
        let operation_coordinator = OperationCoordinator::new();
        diagnostics_nav.replace(std::slice::from_ref(&diagnostics_page));
        let ui = Rc::new_cyclic(|this| BackendUi {
            this: this.clone(),
            model_rows: RefCell::new(Vec::new()),
            model_radio_group: gtk::CheckButton::new(),
            marking_models: std::cell::Cell::new(false),
            preferred_languages: RefCell::new(None),
            controller,
            configurator,
            view_stack: view_stack.clone(),
            backend_nav: backend_nav.clone(),
            diagnostics_nav: diagnostics_nav.clone(),
            overlay: overlay.clone(),
            myna_selector,
            spoken_language,
            client_settings,
            active_backend: ActiveBackendController::with_coordinator(
                crate::domain::ConnectionSnapshot::new(
                    Vec::new(),
                    ActiveBackendState::Disconnected,
                ),
                operation_coordinator.clone(),
            ),
            operation_coordinator,
            diagnostics_page: RefCell::new(Some(diagnostics_page)),
            backend_pages: RefCell::new(BTreeMap::new()),
            apply_state: RefCell::new(BTreeMap::new()),
            shown_state: RefCell::new(None),
            backend_tab_shown: std::cell::Cell::new(
                view_stack.visible_child_name().as_deref() == Some("model"),
            ),
            installed_snaps: RefCell::new(Vec::new()),
            inventory_complete: std::cell::Cell::new(false),
            inventory_failure: RefCell::new(None),
            performance: RefCell::new(None),
            last_diagnostics_refresh: std::cell::Cell::new(None),
            install_dialog: RefCell::new(None),
            model_installs: RefCell::default(),
            install_cancellation: CancellationToken::new(),
            follow_interval: std::cell::Cell::new(FOLLOW_INTERVAL),
            nvidia_gpu: crate::machine::has_nvidia_gpu(),
        });

        ui.connect_view_stack_selection();
        ui.connect_install_button();

        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });

        ui.trigger_discovery();

        ui
    }

    /// Runs `read` off the main thread, since the system reader may block on
    /// the system bus, and recommends a model once it answers.
    pub(crate) fn read_preferred_languages(
        self: &Rc<Self>,
        read: impl FnOnce() -> Vec<String> + Send + 'static,
    ) {
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let languages = gio::spawn_blocking(read).await;
            if let (Some(ui), Ok(languages)) = (ui.upgrade(), languages) {
                ui.set_preferred_languages(languages);
            }
        });
    }

    pub(crate) fn set_preferred_languages(&self, languages: Vec<String>) {
        *self.preferred_languages.borrow_mut() = Some(languages);
        self.render_model_group();
    }

    /// Choosing a model is the go-ahead: snapd's own authorization prompt is
    /// the only question asked.
    fn begin_backend_switch(self: &Rc<Self>, selected: BackendIdentity) {
        let request = match self.active_backend.begin(selected) {
            Ok(request) => request,
            Err(PrepareSwitchError::BackendUnavailable(_)) => {
                self.render_model_group();
                self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                    "This model is no longer installed.",
                )));
                self.trigger_discovery();
                return;
            }
            Err(PrepareSwitchError::Busy) => {
                self.render_model_group();
                return;
            }
        };
        self.render_model_group();
        self.run_backend_switch(request);
    }

    fn run_backend_switch(self: &Rc<Self>, request: crate::active_backend::SwitchRequest) {
        let Some(repository) = self.controller.repository().cloned() else {
            self.active_backend.abandon();
            self.render_model_group();
            return;
        };
        let configurator = Rc::clone(&self.configurator);
        let operation_token = request.operation_token();
        let cancellation = request.cancellation();
        let plan = request.plan().clone();
        let coordinator = self.operation_coordinator.clone();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = execute_switch(
                &plan,
                configurator.as_ref(),
                repository.as_ref(),
                cancellation,
            )
            .await;
            coordinator.complete(operation_token);
            if let Some(ui) = ui.upgrade() {
                if ui.active_backend.complete(operation_token, outcome.clone()) {
                    ui.render_model_group();
                    ui.present_switch_outcome(plan.selected(), &outcome);
                    ui.trigger_discovery();
                }
            }
        });
    }

    /// Only a switch that did not take is announced: a toast naming the
    /// model, whose Details button opens the full report.
    fn present_switch_outcome(&self, target: &BackendIdentity, outcome: &SwitchOutcome) {
        let model = model_family(target.snap_name()).name;
        let heading = match outcome.notice() {
            SwitchNotice::None => return,
            SwitchNotice::Failed => gettextrs::gettext("Changing to {model} failed"),
            SwitchNotice::Unconfirmed => {
                gettextrs::gettext("Changing to {model} could not be confirmed")
            }
        }
        .replace("{model}", &model);
        let (summary, details) = switch_report(outcome);
        self.toast_report(heading, summary, details);
    }

    fn sync_active_backend(self: &Rc<Self>) {
        if !self
            .active_backend
            .set_snapshot(self.controller.connection_snapshot())
        {
            return;
        }
        self.render_model_group();
        self.sync_backend_tab();
    }

    fn sync_backend_streams(&self) {
        let Some(settings) = &self.client_settings else {
            return;
        };
        settings.set_backend_streams(match self.active_backend.snapshot().active_state() {
            ActiveBackendState::Connected(identity) => {
                Some(myna_core::streams_by_default(identity.snap_name()))
            }
            _ => None,
        });
    }

    fn render_model_group(&self) {
        self.sync_backend_streams();
        let Some(page) = self.myna_selector.as_ref() else {
            return;
        };
        let group = page.model_group();
        let snapshot = self.active_backend.snapshot();
        let recommendation = self.recommendation();
        let recommended = recommendation.and_then(|found| found.installed);
        let ordered = recommended_first(snapshot.backends(), recommended);
        let backends = ordered.as_slice();
        page.install_button()
            .set_visible(!installable_families(backends, None).is_empty());
        self.show_install_hint(page, recommendation.and_then(|found| found.better));
        let selectable = backends.len() > 1
            || !matches!(snapshot.active_state(), ActiveBackendState::Connected(_));
        let (listed, listed_selectable) = {
            let rows = self.model_rows.borrow();
            (
                rows.iter()
                    .map(|row| row.backend.clone())
                    .collect::<Vec<_>>(),
                rows.iter().all(|row| row.radio.is_some()),
            )
        };
        if listed.as_slice() != backends || listed_selectable != selectable {
            self.list_models(&group, backends, selectable);
        }
        group.set_visible(!backends.is_empty());

        let status = if !self.active_backend.verified() {
            Some(gettextrs::gettext(
                "Final connections could not be verified. Switching is disabled until refresh succeeds.",
            ))
        } else {
            match snapshot.active_state() {
                ActiveBackendState::Disconnected => Some(gettextrs::gettext(
                    "No model is connected. Choose one to use for dictation.",
                )),
                ActiveBackendState::MultiplyConnected(_) => Some(gettextrs::gettext(
                    "Several models are connected. Choose one to use for dictation.",
                )),
                ActiveBackendState::Connected(_) => None,
            }
        };
        group.set_description(status.map(|status| escape_markup(&status)).as_deref());

        let chosen = self.active_backend.chosen();
        let switching_to = self.active_backend.switching_to();
        let user_language = self.user_language();
        self.marking_models.set(true);
        for row in self.model_rows.borrow().iter() {
            if let Some(radio) = &row.radio {
                radio.set_active(chosen.as_ref() == Some(&row.backend));
            }
            let family = myna_core::language::ModelFamily::from_snap_name(row.backend.snap_name());
            if let (Some(languages), Some(family)) = (&row.languages, family) {
                show_languages(languages, family, user_language.as_deref());
            }
            let recommended = is_recommended(&row.backend, recommended);
            if let (true, Some(family)) = (recommended, family) {
                set_named(
                    &row.pill,
                    &recommendation_label(family, user_language.as_deref()),
                );
            }
            row.pill.set_visible(recommended);
            let subtitle = row.row.subtitle();
            let pill = row.pill.label();
            match model_description(subtitle.as_deref(), recommended.then_some(pill.as_str())) {
                Some(description) => row
                    .focus_target()
                    .update_property(&[gtk::accessible::Property::Description(&description)]),
                None => row
                    .focus_target()
                    .reset_property(gtk::AccessibleProperty::Description),
            }
            let switching = switching_to.as_ref() == Some(&row.backend);
            row.spinner.set_visible(switching);
            row.spinner.set_spinning(switching);
        }
        self.marking_models.set(false);

        let apply_active = self.operation_coordinator.active() == Some(OperationKind::BackendApply);
        group.set_sensitive(
            self.active_backend.verified() && !apply_active && switching_to.is_none(),
        );
        self.sync_install_dialog();
    }

    /// One row per installed backend, named and described by family, with a
    /// radio to choose it when there is a choice to make.
    fn list_models(
        &self,
        group: &adw::PreferencesGroup,
        backends: &[BackendIdentity],
        selectable: bool,
    ) {
        for row in self.model_rows.take() {
            group.remove(&row.row);
        }
        let mut rows = Vec::new();
        for backend in backends {
            let family = model_family(backend.snap_name());
            let row = adw::ActionRow::builder()
                .title(&family.name)
                .use_markup(false)
                .build();
            if let Some(description) = &family.description {
                row.set_subtitle(description);
            }
            let radio = selectable.then(|| {
                let radio = gtk::CheckButton::builder()
                    .accessible_role(gtk::AccessibleRole::Radio)
                    .valign(gtk::Align::Center)
                    .build();
                radio.set_group(Some(&self.model_radio_group));
                radio.update_property(&[gtk::accessible::Property::Label(&family.name)]);
                radio.connect_toggled({
                    let ui = self.this.clone();
                    let backend = backend.clone();
                    move |radio| {
                        let Some(ui) = ui.upgrade() else {
                            return;
                        };
                        if radio.is_active() && !ui.marking_models.get() {
                            ui.begin_backend_switch(backend.clone());
                        }
                    }
                });
                radio
            });
            let pill = recommended_pill();
            pill.set_visible(false);
            let spinner = gtk::Spinner::builder()
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            spinner.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
                "Changing model",
            ))]);
            if let Some(radio) = &radio {
                row.add_prefix(radio);
                row.set_activatable_widget(Some(radio));
                radio.reset_relation(gtk::AccessibleRelation::DescribedBy);
            } else {
                row.reset_relation(gtk::AccessibleRelation::DescribedBy);
            }
            let languages = myna_core::language::ModelFamily::from_snap_name(backend.snap_name())
                .map(|family| {
                    let ui = self.this.clone();
                    languages_button(family, move || {
                        ui.upgrade().and_then(|ui| ui.user_language())
                    })
                });
            row.add_suffix(&pill);
            if let Some(languages) = &languages {
                row.add_suffix(languages);
            }
            row.add_suffix(&spinner);
            group.add(&row);
            rows.push(ModelRow {
                backend: backend.clone(),
                row,
                radio,
                pill,
                languages,
                spinner,
            });
        }
        *self.model_rows.borrow_mut() = rows;
    }

    /// The recommendation among the installed models, once the language is
    /// known.
    fn recommendation(&self) -> Option<Recommendation> {
        let snapshot = self.active_backend.snapshot();
        self.preferred_languages
            .borrow()
            .as_deref()
            .map(|preferred| recommendation(preferred, snapshot.backends()))
    }

    /// The second line of Install more models: the install running from its
    /// dialog, else that `better` would serve the user's language better
    /// than anything installed.
    fn show_install_hint(
        &self,
        page: &ui::MynaPage,
        better: Option<myna_core::language::ModelFamily>,
    ) {
        let button = page.install_button();
        let hint = page.install_hint();
        let running =
            self.model_installs
                .borrow()
                .iter()
                .find_map(|(family, install)| match install {
                    ModelInstall::Running(percent) => Some((*family, *percent)),
                    ModelInstall::Confirming => None,
                });
        let text = match (running, better) {
            (Some((family, percent)), _) => Some(installing_hint(family, percent)),
            (None, Some(family)) => {
                Some(better_model_hint(family, self.user_language().as_deref()))
            }
            (None, None) => None,
        };
        match &text {
            Some(named) => {
                set_named(&hint, named);
                button.update_property(&[gtk::accessible::Property::Description(&hint.label())]);
            }
            None => button.reset_property(gtk::AccessibleProperty::Description),
        }
        hint.set_visible(text.is_some());
    }

    /// The language the recommendation is for, once it is known.
    fn user_language(&self) -> Option<String> {
        self.preferred_languages
            .borrow()
            .as_deref()
            .and_then(myna_core::language::user_language)
    }

    fn connect_install_button(self: &Rc<Self>) {
        let Some(page) = self.myna_selector.as_ref() else {
            return;
        };
        page.install_button().connect_clicked({
            let ui = Rc::downgrade(self);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.present_install_models();
                }
            }
        });
    }

    fn install_offer(&self) -> InstallOffer {
        let recommended = self.recommendation().and_then(|found| found.better);
        InstallOffer {
            families: installable_families(self.active_backend.snapshot().backends(), recommended),
            recommended,
            user_language: self.user_language(),
        }
    }

    fn install_callback(&self) -> InstallFamily {
        let ui = self.this.clone();
        Rc::new(move |family| {
            if let Some(ui) = ui.upgrade() {
                ui.install_family(family);
            }
        })
    }

    fn install_models(&self) -> ui::InstallModelsDialog {
        let dialog = ui::InstallModelsDialog::new();
        let shown = self.install_offer();
        let rows = offer_install(&dialog, &shown, self.nvidia_gpu, self.install_callback());
        *self.install_dialog.borrow_mut() = Some(OpenInstallDialog {
            dialog: dialog.downgrade(),
            shown,
            rows,
        });
        self.render_installs();
        dialog
    }

    fn present_install_models(self: &Rc<Self>) {
        let dialog = self.install_models();
        dialog.connect_closed({
            let ui = self.this.clone();
            move |closed| {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                let mut open = ui.install_dialog.borrow_mut();
                if open
                    .as_ref()
                    .is_some_and(|open| open.dialog.upgrade().as_ref() == Some(closed))
                {
                    open.take();
                }
            }
        });
        dialog.present(Some(self.overlay.upcast_ref::<gtk::Widget>()));
        self.follow_installs_elsewhere();
    }

    /// Keeps an open Install more models dialog listing what it opened with
    /// and anything missing since, with the recommendation's pill where it
    /// now belongs.
    fn sync_install_dialog(&self) {
        let offer = self.install_offer();
        let rebuild = {
            let mut open = self.install_dialog.borrow_mut();
            let Some(open) = open.as_mut() else {
                return;
            };
            let wanted = InstallOffer {
                families: listed_families(&open.shown.families, &offer.families, offer.recommended),
                ..offer
            };
            if open.shown == wanted {
                None
            } else {
                open.shown = wanted.clone();
                open.dialog.upgrade().map(|dialog| (dialog, wanted))
            }
        };
        if let Some((dialog, wanted)) = rebuild {
            let rows = offer_install(&dialog, &wanted, self.nvidia_gpu, self.install_callback());
            if let Some(open) = self.install_dialog.borrow_mut().as_mut() {
                open.rows = rows;
            }
        }
        self.render_installs();
    }

    /// Show where each install stands: on its row in the open dialog, and on
    /// Install more models, which says what is installing while it runs.
    fn render_installs(&self) {
        if let Some(page) = self.myna_selector.as_ref() {
            let better = self.recommendation().and_then(|found| found.better);
            self.show_install_hint(page, better);
        }
        let open = self.install_dialog.borrow();
        let Some(open) = open.as_ref() else {
            return;
        };
        let installs = self.model_installs.borrow();
        let running = installs
            .values()
            .any(|install| matches!(install, ModelInstall::Running(_)));
        let snapshot = self.active_backend.snapshot();
        for offered in &open.rows {
            let install = installs.get(&offered.family).copied();
            offered
                .row
                .update_state(&[gtk::accessible::State::Busy(install.is_some())]);
            let installed = snapshot
                .backends()
                .iter()
                .any(|backend| backend.snap_name() == offered.family.snap_name());
            match install {
                Some(ModelInstall::Running(percent)) => offered.control.show_installing(percent),
                Some(ModelInstall::Confirming) => offered.control.show_installing(None),
                None if installed => offered.control.show_installed(),
                None => {
                    let name = model_family(offered.family.snap_name()).name;
                    offered.control.show_offer(
                        &gettextrs::gettext("Install"),
                        // TRANSLATORS: the Install button's name to assistive technology; {model} is a model family, such as "Whisper".
                        &gettextrs::gettext("Install {model}").replace("{model}", &name),
                    );
                    // One snapd install at a time: its prompt covers one request.
                    offered.control.button().set_sensitive(!running);
                }
            }
        }
    }

    /// Install `family` through snapd as the user, one at a time, as the
    /// onboarding rows do. Its polkit prompt is the only question:
    /// dismissing it puts the button back silently, a refusal or a failed
    /// change with a toast whose Details open the report.
    fn install_family(self: &Rc<Self>, family: myna_core::language::ModelFamily) {
        let busy = self
            .model_installs
            .borrow()
            .values()
            .any(|install| matches!(install, ModelInstall::Running(_)));
        if busy {
            return;
        }
        self.model_installs
            .borrow_mut()
            .insert(family, ModelInstall::Running(None));
        self.render_installs();
        let offer = family_offer(family, self.nvidia_gpu);
        self.follow_install(family, move |configurator, follow, report| {
            Box::pin(async move {
                crate::snap_install::install(
                    configurator.as_ref(),
                    family.snap_name(),
                    offer.download_bytes,
                    &follow,
                    &report,
                )
                .await
            })
        });
    }

    /// Follow an install of an offered family that snapd is already running,
    /// started in App Center or a terminal, rather than offer to start it
    /// again. One read of snapd's changes each time the dialog opens.
    fn follow_installs_elsewhere(self: &Rc<Self>) {
        let offered: Vec<_> = self
            .install_dialog
            .borrow()
            .as_ref()
            .map(|open| open.shown.families.clone())
            .unwrap_or_default();
        if offered.is_empty() {
            return;
        }
        let ui = Rc::downgrade(self);
        let configurator = self.configurator.clone();
        glib::spawn_future_local(async move {
            let Ok(changes) = configurator
                .changes_in_progress(CancellationToken::new())
                .await
            else {
                return;
            };
            let Some(ui) = ui.upgrade() else {
                return;
            };
            for family in offered {
                if ui.model_installs.borrow().contains_key(&family) {
                    continue;
                }
                let Some(change) =
                    crate::snap_changes::pending_install(&changes, family.snap_name())
                else {
                    continue;
                };
                let change_id = change.id().to_owned();
                ui.model_installs
                    .borrow_mut()
                    .insert(family, ModelInstall::Running(None));
                let offer = family_offer(family, ui.nvidia_gpu);
                ui.follow_install(family, move |configurator, follow, report| {
                    Box::pin(async move {
                        crate::snap_install::follow_change(
                            configurator.as_ref(),
                            &change_id,
                            offer.download_bytes,
                            &follow,
                            &report,
                        )
                        .await
                        // Not the user's action, so a failure only puts
                        // Install back.
                        .map_err(|_| crate::ports::SystemConfiguratorError::Cancelled)
                    })
                });
            }
            ui.render_installs();
        });
    }

    /// Run `run`, an install or the following of one, reporting its progress
    /// on `family`'s row, then rediscover so the model lists.
    fn follow_install<F>(self: &Rc<Self>, family: myna_core::language::ModelFamily, run: F)
    where
        F: FnOnce(
                Rc<dyn SystemConfigurator>,
                crate::snap_install::Follow<'static>,
                Box<dyn Fn(Option<u8>)>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                        Output = Result<(), crate::ports::SystemConfiguratorError>,
                    >,
                >,
            > + 'static,
    {
        let ui = Rc::downgrade(self);
        let follow = crate::snap_install::Follow {
            interval: self.follow_interval.get(),
            sleep: &glib_sleep,
            cancellation: self.install_cancellation.clone(),
        };
        let report: Box<dyn Fn(Option<u8>)> = Box::new({
            let ui = ui.clone();
            move |percent| {
                if let Some(ui) = ui.upgrade() {
                    ui.model_installs
                        .borrow_mut()
                        .insert(family, ModelInstall::Running(percent));
                    ui.render_installs();
                }
            }
        });
        let outcome = run(self.configurator.clone(), follow, report);
        glib::spawn_future_local(async move {
            let outcome = outcome.await;
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let name = model_family(family.snap_name()).name;
            match outcome {
                Ok(()) => {
                    glib::g_message!(
                        crate::LOG_DOMAIN,
                        "install: {} installed",
                        family.snap_name()
                    );
                    ui.model_installs
                        .borrow_mut()
                        .insert(family, ModelInstall::Confirming);
                    ui.trigger_discovery();
                }
                Err(crate::ports::SystemConfiguratorError::Cancelled) => {
                    ui.model_installs.borrow_mut().remove(&family);
                }
                Err(error) => {
                    glib::g_message!(
                        crate::LOG_DOMAIN,
                        "install: {} failed: {}",
                        family.snap_name(),
                        error
                    );
                    ui.model_installs.borrow_mut().remove(&family);
                    // TRANSLATORS: {model} is a model family, such as "Whisper".
                    let heading =
                        gettextrs::gettext("Installing {model} failed").replace("{model}", &name);
                    ui.toast_report(
                        heading,
                        system_failure_summary(&error),
                        system_error_details(&error),
                    );
                }
            }
            ui.render_installs();
        });
    }

    /// A toast whose Details button opens the full report.
    fn toast_report(&self, heading: String, summary: String, details: String) {
        let toast = adw::Toast::builder()
            .title(escape_markup(&heading))
            .button_label(gettextrs::gettext("Details"))
            .build();
        toast.connect_button_clicked({
            let overlay = self.overlay.clone();
            move |_| {
                ui::OperationErrorDialog::new(&heading, &summary, &details)
                    .present(Some(overlay.upcast_ref::<gtk::Widget>()));
            }
        });
        self.overlay.add_toast(toast);
    }

    fn connect_view_stack_selection(self: &Rc<Self>) {
        self.view_stack.connect_visible_child_name_notify({
            let ui = Rc::downgrade(self);
            move |stack| {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                let tab = stack.visible_child_name();
                let backend_shown = tab.as_deref() == Some("model");
                let shown = ui.shown_backend();
                if ui.backend_tab_shown.replace(backend_shown) && !backend_shown {
                    if let Some(name) = &shown {
                        ui.controller.cancel_page(name);
                    }
                }
                match tab.as_deref() {
                    Some("diagnostics") => ui.on_diagnostics_page_shown(),
                    Some("model") => {
                        if let Some(name) = shown {
                            ui.trigger_snapshot(&name);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    /// The settings window's own actions: `win.setup` reopens the onboarding
    /// wizard over it, and `win.refresh` refreshes the tab on show. Regaining
    /// focus rediscovers the installed models.
    pub fn install_window_actions(self: &Rc<Self>, window: &ui::MainWindow) {
        let refresh = gio::SimpleAction::new("refresh", None);
        refresh.connect_activate({
            let ui = Rc::downgrade(self);
            move |_, _| {
                if let Some(ui) = ui.upgrade() {
                    ui.refresh_visible_tab();
                }
            }
        });
        window.add_action(&refresh);

        let setup = gio::SimpleAction::new("setup", None);
        let gate = Rc::new(SetupGate::new(setup.clone()));
        setup.connect_activate({
            let ui = Rc::downgrade(self);
            let window = window.downgrade();
            let gate = gate.clone();
            move |_, _| {
                if let (Some(ui), Some(window)) = (ui.upgrade(), window.upgrade()) {
                    ui.open_setup(&gate, &window);
                }
            }
        });
        window.add_action(&setup);

        window.connect_is_active_notify({
            let ui = Rc::downgrade(self);
            move |window| {
                if let (true, Some(ui)) = (window.is_active(), ui.upgrade()) {
                    ui.rediscover_on_focus(Instant::now());
                }
            }
        });
    }

    /// Assess the machine and open the wizard on it. Closing the wizard
    /// rediscovers, since it may have connected a backend or restarted the
    /// daemon.
    fn open_setup(self: &Rc<Self>, gate: &Rc<SetupGate>, window: &ui::MainWindow) {
        if self.operation_coordinator.active().is_some() {
            self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                "Wait for the current change to finish.",
            )));
            return;
        }
        let Some(repository) = self.controller.repository().cloned() else {
            return;
        };
        let Some(application) = window
            .application()
            .and_then(|application| application.downcast::<adw::Application>().ok())
        else {
            return;
        };
        gate.set_wizard_open(true);
        let configurator = self.configurator.clone();
        let ui = Rc::downgrade(self);
        let gate = gate.clone();
        let window = window.clone();
        glib::spawn_future_local(async move {
            let desktop = crate::platform::Platform::current().components();
            let components = crate::onboarding_ui::assess_machine(
                repository.as_ref(),
                configurator.as_ref(),
                desktop.as_ref(),
            )
            .await;
            if ui.upgrade().is_none() {
                gate.set_wizard_open(false);
                return;
            }
            let wizard = crate::onboarding_ui::OnboardingUi::present_with_ports(
                &application,
                components,
                repository,
                configurator,
                desktop,
                crate::onboarding_ui::Opener::Settings(window.upcast_ref()),
            );
            wizard.window().connect_close_request(move |_| {
                gate.set_wizard_open(false);
                if let Some(ui) = ui.upgrade() {
                    ui.rediscover();
                }
                glib::Propagation::Proceed
            });
        });
    }

    /// Through the tab's own refresh, so its gating applies: a backend page
    /// refuses while it loads or applies, diagnostics debounce.
    fn refresh_visible_tab(self: &Rc<Self>) {
        match self.view_stack.visible_child_name().as_deref() {
            Some("diagnostics") => self.on_diagnostics_requested(),
            Some("model") => {
                if let Some(page) = self.backend_nav.visible_page() {
                    let _ = WidgetExt::activate_action(&page, "backend.refresh", None);
                }
            }
            _ => {}
        }
    }

    /// The user may have installed or removed a model in App Center or a
    /// terminal while the window was in the background. Never during an
    /// operation: a switch or apply rediscovers when it completes.
    fn rediscover_on_focus(self: &Rc<Self>, at: Instant) {
        if self.operation_coordinator.active().is_some() {
            return;
        }
        if let Some(request) = self.controller.begin_focus_discovery(at) {
            self.run_discovery(request);
        }
    }

    fn rediscover(self: &Rc<Self>) {
        if !self.controller.discovery_loading() {
            self.trigger_discovery();
        }
    }

    pub(crate) fn operation_coordinator(&self) -> &OperationCoordinator {
        &self.operation_coordinator
    }

    pub fn controller(&self) -> Rc<BackendController> {
        Rc::clone(&self.controller)
    }

    fn apply_state_view(&self, snap_name: &str) -> Option<ApplyingView> {
        self.apply_state
            .borrow()
            .get(snap_name)
            .map(|change| ApplyingView {
                key: change.key.clone(),
                value: change.value.clone(),
                progress: change.progress().to_owned(),
            })
    }

    /// A row's widget reports the user's change here; it applies once the
    /// widget's own signal has returned, since applying rebuilds the page.
    fn request_change(self: &Rc<Self>, snap_name: &str, key: &str, value: ConfigValue) {
        let ui = Rc::downgrade(self);
        let (snap_name, key) = (snap_name.to_owned(), key.to_owned());
        glib::idle_add_local_once(move || {
            if let Some(ui) = ui.upgrade() {
                ui.apply_change(&snap_name, &key, value);
            }
        });
    }

    /// Applies one change the user made: the polkit prompt is the only
    /// question. A change that cannot start puts the row back.
    fn apply_change(self: &Rc<Self>, snap_name: &str, key: &str, value: ConfigValue) {
        let Some(page) = self.controller.page(snap_name) else {
            return;
        };
        if self.apply_state.borrow().contains_key(snap_name) {
            self.rebuild_backend_page(snap_name);
            return;
        }
        let preview = match prepare_change(&page, key, value.clone()) {
            Ok(preview) => preview,
            Err(PrepareApplyError::NoChanges) => return,
            Err(PrepareApplyError::Invalid(issues)) => {
                self.rebuild_backend_page(snap_name);
                self.overlay
                    .add_toast(adw::Toast::new(&escape_markup(&validation_issue_summary(
                        &issues,
                    ))));
                return;
            }
        };
        let Ok(operation) = self
            .operation_coordinator
            .begin(OperationKind::BackendApply)
        else {
            self.rebuild_backend_page(snap_name);
            self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                "Wait for the current change to finish.",
            )));
            return;
        };
        let focus = self.backend_focus(snap_name).map(|focus| BackendFocus {
            entry: None,
            ..focus
        });
        self.apply_state.borrow_mut().insert(
            snap_name.to_owned(),
            PendingChange {
                key: key.to_owned(),
                value,
                operation_token: operation.token(),
                cancellation: operation.cancellation(),
                progress_message: gettextrs::gettext("Applying and restarting the model…"),
                progress_detail: None,
                focus,
            },
        );
        self.rebuild_backend_page(snap_name);
        self.render_model_group();
        self.run_apply(preview, operation.token(), operation.cancellation());
    }

    fn run_apply(
        self: &Rc<Self>,
        preview: ApplyPreview,
        operation_token: u64,
        cancellation: CancellationToken,
    ) {
        let snap_name = preview.backend().snap_name().to_owned();
        let Some(repository) = self.controller.repository().cloned() else {
            self.operation_coordinator.abandon(operation_token);
            self.apply_state.borrow_mut().remove(&snap_name);
            self.rebuild_backend_page(&snap_name);
            self.render_model_group();
            return;
        };
        let configurator = Rc::clone(&self.configurator);
        let watching = CancellationToken::new();
        self.watch_apply_progress(&snap_name, watching.clone());
        let coordinator = self.operation_coordinator.clone();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = execute_backend_apply(
                &preview,
                configurator.as_ref(),
                repository.as_ref(),
                cancellation,
            )
            .await;
            watching.cancel();
            coordinator.complete(operation_token);
            if let Some(ui) = ui.upgrade() {
                ui.complete_apply(result, &snap_name);
            }
        });
    }

    /// Polls snapd about once a second while the apply runs, over the socket
    /// and as the user, so the row says what the apply is waiting on: a model
    /// download above all, which can take minutes.
    fn watch_apply_progress(self: &Rc<Self>, snap_name: &str, watching: CancellationToken) {
        let ui = Rc::downgrade(self);
        let configurator = Rc::clone(&self.configurator);
        let snap_name = snap_name.to_owned();
        glib::spawn_future_local(async move {
            while !watching.is_cancelled() {
                let progress = configurator
                    .apply_progress(&snap_name, watching.clone())
                    .await;
                if watching.is_cancelled() {
                    break;
                }
                let Some(ui) = ui.upgrade() else {
                    break;
                };
                ui.show_apply_progress(&snap_name, progress.as_ref().map(apply_progress_text));
                drop(ui);
                glib::timeout_future(APPLY_PROGRESS_INTERVAL).await;
            }
        });
    }

    /// Updates the changed row in place: rebuilding the page every second
    /// would reset its scroll and focus.
    fn show_apply_progress(&self, snap_name: &str, detail: Option<String>) {
        let (key, text) = {
            let mut states = self.apply_state.borrow_mut();
            let Some(change) = states.get_mut(snap_name) else {
                return;
            };
            if change.progress_detail == detail {
                return;
            }
            change.progress_detail = detail;
            (change.key.clone(), change.progress().to_owned())
        };
        let Some(widget) = self.backend_pages.borrow().get(snap_name).cloned() else {
            return;
        };
        let Some(row) = find_named_descendant(widget.upcast_ref(), &setting_widget_name(&key))
        else {
            return;
        };
        match row.downcast::<adw::ActionRow>() {
            Ok(row) => row.set_subtitle(&escape_markup(&text)),
            Err(row) => {
                if let Some(label) = find_named_descendant(&row, APPLY_PROGRESS)
                    .and_then(|label| label.downcast::<gtk::Label>().ok())
                {
                    label.set_label(&text);
                }
            }
        }
    }

    /// The change is over: the row shows what the model read back. Only a
    /// change that did not take is announced, in a toast naming the setting
    /// whose Details button opens the full report.
    fn complete_apply(
        self: &Rc<Self>,
        result: Result<ApplySuccess, ApplyFailure>,
        snap_name: &str,
    ) {
        let Some(change) = self.apply_state.borrow_mut().remove(snap_name) else {
            return;
        };
        let Some(page) = self.controller.page(snap_name) else {
            self.render_model_group();
            return;
        };
        let setting = page
            .rows()
            .iter()
            .find(|row| row.key() == change.key)
            .map_or_else(
                || change.key.clone(),
                |row| row.metadata().title().to_owned(),
            );
        let (snapshot, notice) = apply_report(result);
        match snapshot {
            Some(snapshot) => self.controller.apply_readback(snap_name, snapshot),
            None => self.rebuild_backend_page(snap_name),
        }
        if let (Some(focus), true) = (
            change.focus,
            self.shown_backend().as_deref() == Some(snap_name),
        ) {
            self.restore_backend_focus(&focus);
        }
        self.render_model_group();
        if let Some(notice) = notice {
            self.present_apply_notice(&setting, notice);
        }
    }

    fn present_apply_notice(&self, setting: &str, notice: ApplyNotice) {
        let frame = if notice.failed {
            // TRANSLATORS: {setting} is a setting's name on the Model tab, such as "Unload when idle (seconds)".
            gettextrs::gettext("Changing “{setting}” failed")
        } else {
            // TRANSLATORS: {setting} is a setting's name on the Model tab, such as "Unload when idle (seconds)".
            gettextrs::gettext("Changing “{setting}” could not be confirmed")
        };
        let heading = frame.replace("{setting}", setting);
        self.toast_report(heading, notice.summary, notice.details);
    }

    fn on_controller_event(self: &Rc<Self>, event: &ControllerEvent) {
        match event {
            ControllerEvent::DiscoveryStarted => {
                self.inventory_complete.set(false);
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::DiscoveryChanged => {
                self.prune_missing_backends();
                self.sync_active_backend();
                for page in self.controller.pages() {
                    self.rebuild_backend_page(page.identity().snap_name());
                }
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::BackendChanged(identity) => {
                if self.controller.page(identity.snap_name()).is_some() {
                    self.render_model_group();
                }
                self.rebuild_backend_page(identity.snap_name());
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::DiscoveryFailed(error) => {
                self.rebuild_diagnostics_page();
                // The cause is on the Diagnostics page, rebuilt above.
                glib::g_warning!(
                    crate::LOG_DOMAIN,
                    "discovery failed: {}",
                    diagnostics::redact_text(error.message())
                );
                self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                    "Could not read the installed models.",
                )));
            }
        }
    }

    /// Drops cached pages and staged-apply state for backends that
    /// disappeared from discovery. The Model tab itself is kept in sync
    /// separately by [`Self::sync_backend_tab`].
    fn prune_missing_backends(self: &Rc<Self>) {
        let known: Vec<String> = self
            .controller
            .pages()
            .iter()
            .map(|page| page.identity().snap_name().to_owned())
            .collect();
        let stale: Vec<String> = self
            .backend_pages
            .borrow()
            .keys()
            .filter(|name| !known.contains(*name))
            .cloned()
            .collect();
        for name in stale {
            self.backend_pages.borrow_mut().remove(&name);
            cancel_apply_state(
                &mut self.apply_state.borrow_mut(),
                &self.operation_coordinator,
                &name,
            );
            self.controller.cancel_page(&name);
        }
    }

    /// Keeps the Model tab showing the single active backend, or why there
    /// is none. Called whenever the active-backend snapshot changes.
    fn sync_backend_tab(self: &Rc<Self>) {
        let state = self.active_backend.snapshot().active_state();
        if self.shown_state.borrow().as_ref() == Some(&state) {
            return;
        }
        if let Some(name) = self.shown_backend() {
            self.controller.cancel_page(&name);
        }
        *self.shown_state.borrow_mut() = Some(state.clone());
        let (title, description) = match state {
            ActiveBackendState::Connected(identity) => {
                let name = identity.snap_name();
                self.rebuild_backend_page(name);
                if self.backend_tab_shown.get() {
                    self.trigger_snapshot(name);
                }
                return;
            }
            ActiveBackendState::Disconnected => (
                gettextrs::gettext("No active model"),
                gettextrs::gettext("Choose a model on the General tab."),
            ),
            ActiveBackendState::MultiplyConnected(_) => (
                gettextrs::gettext("Several models are connected"),
                gettextrs::gettext("Choose one model on the General tab."),
            ),
        };
        self.detach_spoken_language();
        let page = model_status_page(&title, &description, "emblem-system-symbolic");
        self.backend_nav.replace(&[page]);
    }

    /// The spoken-language group, off any page, when `snap_name`'s family
    /// takes it, for the shown backend's page to show.
    fn spoken_language_for(&self, snap_name: &str) -> Option<adw::PreferencesGroup> {
        self.detach_spoken_language();
        let takes = myna_core::language::ModelFamily::from_snap_name(snap_name)
            .is_some_and(|family| family.takes_spoken_language());
        takes.then(|| self.spoken_language.clone()).flatten()
    }

    fn detach_spoken_language(&self) {
        let Some(group) = &self.spoken_language else {
            return;
        };
        if let Some(page) = group
            .ancestor(adw::PreferencesPage::static_type())
            .and_downcast::<adw::PreferencesPage>()
        {
            page.remove(group);
        }
    }

    /// The snap whose page the Model tab shows, if any.
    fn shown_backend(&self) -> Option<String> {
        match self.shown_state.borrow().as_ref() {
            Some(ActiveBackendState::Connected(identity)) => Some(identity.snap_name().to_owned()),
            _ => None,
        }
    }

    fn rebuild_backend_page(self: &Rc<Self>, snap_name: &str) {
        let focus = self.backend_focus(snap_name);
        let Some(page) = self.controller.page(snap_name) else {
            return;
        };
        // The page is kept and refilled, so it stays where it was scrolled.
        let widget = self
            .backend_pages
            .borrow_mut()
            .entry(snap_name.to_owned())
            .or_default()
            .clone();
        if focus.is_some() {
            // Removed with the focus, a row makes GTK 4.14 focus the next
            // one it finds and scroll there.
            if let Some(window) = self.backend_nav.root().and_downcast::<gtk::Window>() {
                gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            }
        }
        populate_backend_page(&widget, &page, self);
        if self.shown_backend().as_deref() == Some(snap_name) {
            let widget = widget.upcast::<adw::NavigationPage>();
            if self.backend_nav.visible_page().as_ref() != Some(&widget) {
                self.backend_nav.replace(&[widget]);
            }
            if let Some(focus) = focus {
                self.restore_backend_focus(&focus);
            }
        }
    }

    fn backend_focus(&self, snap_name: &str) -> Option<BackendFocus> {
        let page = self.backend_pages.borrow().get(snap_name)?.clone();
        let window = self.backend_nav.root()?.downcast::<gtk::Window>().ok()?;
        let mut current = gtk::prelude::GtkWindowExt::focus(&window);
        while let Some(widget) = current {
            let widget_name = widget.widget_name();
            if widget_name.starts_with("myna-setting-") {
                let entry = widget
                    .clone()
                    .downcast::<adw::EntryRow>()
                    .ok()
                    .map(|entry| EntryFocus {
                        text: entry.text(),
                        cursor_position: entry.position(),
                    });
                return Some(BackendFocus { widget_name, entry });
            }
            if widget == page.clone().upcast::<gtk::Widget>() {
                break;
            }
            current = widget.parent();
        }
        None
    }

    fn restore_backend_focus(&self, focus: &BackendFocus) {
        let Some(page) = self.backend_nav.visible_page() else {
            return;
        };
        let Some(widget) = find_named_descendant(page.upcast_ref(), &focus.widget_name)
            .filter(|widget| widget.can_focus() && widget.is_sensitive())
        else {
            return;
        };
        // The rebuilt row is where the old one was, but not yet laid out:
        // scrolling to it now would scroll to nowhere.
        let viewport = widget
            .ancestor(gtk::Viewport::static_type())
            .and_downcast::<gtk::Viewport>();
        if let Some(viewport) = &viewport {
            viewport.set_scroll_to_focus(false);
        }
        match widget.downcast::<adw::EntryRow>() {
            Ok(entry) => {
                // Focusing an entry selects its text, which the user did not.
                if let Some(text) = descendants(entry.upcast_ref())
                    .into_iter()
                    .find_map(|widget| widget.downcast::<gtk::Text>().ok())
                {
                    text.grab_focus_without_selecting();
                }
                match focus.entry.as_ref() {
                    // Text the user has not applied yet survives a rebuild.
                    Some(focus) => {
                        entry.set_text(&focus.text);
                        entry.set_position(focus.cursor_position);
                    }
                    None => entry.set_position(-1),
                }
            }
            Err(widget) => {
                widget.grab_focus();
            }
        }
        if let Some(viewport) = &viewport {
            viewport.set_scroll_to_focus(true);
        }
    }

    fn rebuild_diagnostics_page(self: &Rc<Self>) {
        let focus = self.diagnostics_focus();
        let page = self.build_diagnostics_page();
        *self.diagnostics_page.borrow_mut() = Some(page.clone());
        self.diagnostics_nav.replace(&[page]);
        self.restore_diagnostics_focus(focus);
    }

    fn diagnostics_focus(&self) -> Option<DiagnosticsFocus> {
        let page = self
            .diagnostics_page
            .borrow()
            .as_ref()?
            .clone()
            .downcast::<ui::DiagnosticsPage>()
            .ok()?;
        let window = self.overlay.root()?.downcast::<gtk::Window>().ok()?;
        let focus = gtk::prelude::GtkWindowExt::focus(&window)?;
        if focus == page.copy_button().upcast::<gtk::Widget>() {
            Some(DiagnosticsFocus::Copy)
        } else if focus == page.refresh_button().upcast::<gtk::Widget>() {
            Some(DiagnosticsFocus::Refresh)
        } else if focus == page.report_view().upcast::<gtk::Widget>() {
            Some(DiagnosticsFocus::Report)
        } else {
            None
        }
    }

    fn restore_diagnostics_focus(&self, target: Option<DiagnosticsFocus>) {
        let Some(target) = target else {
            return;
        };
        let Some(page) = self
            .diagnostics_page
            .borrow()
            .as_ref()
            .and_then(|page| page.clone().downcast::<ui::DiagnosticsPage>().ok())
        else {
            return;
        };
        match target {
            DiagnosticsFocus::Copy => page.copy_button().grab_focus(),
            DiagnosticsFocus::Refresh if page.refresh_button().is_sensitive() => {
                page.refresh_button().grab_focus()
            }
            DiagnosticsFocus::Refresh => page.copy_button().grab_focus(),
            DiagnosticsFocus::Report => page.report_view().grab_focus(),
        };
    }

    fn build_diagnostics_page(self: &Rc<Self>) -> adw::NavigationPage {
        let widget = ui::DiagnosticsPage::new();

        let pages = self.controller.pages();
        let discovery_error = self.controller.last_discovery_error();
        let discovery_loading = self.controller.discovery_loading();

        let snaps = self.installed_snaps.borrow().clone();
        let desktop = crate::platform::Platform::current().diagnostics();
        let input = DiagnosticInput {
            inventory_complete: self.inventory_complete.get(),
            machine: Some(crate::machine::machine_facts()),
            daemon: crate::machine::snap_process("myna"),
            daemon_report: crate::machine::daemon_report(),
            extension: desktop.extension,
            desktop: desktop.components,
            performance: self.performance.borrow().clone(),
            backends: pages
                .iter()
                .map(|page| backend_diagnostic_from(page, &snaps))
                .collect(),
            problems: self
                .inventory_failure
                .borrow()
                .iter()
                .cloned()
                .chain(discovery_error.as_ref().map(problem_from_surface_error))
                .collect(),
            installed_snaps: snaps,
        };

        let report = present_diagnostics(input);

        // Header buttons: refresh + copy.
        let refresh_button = widget.refresh_button();
        let copy_button = widget.copy_button();

        refresh_button.set_sensitive(!discovery_loading);
        refresh_button.set_tooltip_text(Some(&gettextrs::gettext("Refresh diagnostics")));
        refresh_button.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Refresh diagnostics",
        ))]);
        refresh_button.connect_clicked({
            let ui = Rc::downgrade(self);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_diagnostics_requested();
                }
            }
        });

        copy_button.set_tooltip_text(Some(&gettextrs::gettext("Copy privacy-safe diagnostics")));
        copy_button.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Copy diagnostics",
        ))]);
        {
            let text = report.copy_text();
            let overlay = self.overlay.clone();
            copy_button.connect_clicked(move |button| {
                let display = button.display();
                display.clipboard().set_text(&text);
                overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                    "Diagnostics copied to clipboard",
                )));
            });
        }

        // Warnings first: the page exists to say why dictation is slow.
        let warnings_group = widget.warnings_group();
        warnings_group.set_visible(!report.warnings().is_empty());
        for warning in report.warnings() {
            let row = adw::ActionRow::builder()
                .title(escape_markup(&warning.cause))
                .subtitle(escape_markup(&warning.remedy))
                .build();
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.add_css_class("warning");
            row.add_prefix(&icon);
            warnings_group.add(&row);
        }

        // Fill the read-only report view.
        let report_view = widget.report_view();
        report_view.buffer().set_text(&report.copy_text());
        hang_continuations(&report_view, report.continuation_columns());
        report_view.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Diagnostic report",
        ))]);

        // What is missing installs from the wizard, never from a terminal.
        let missing = match report.onboarding() {
            OnboardingState::NoMyna => Some(gettextrs::gettext("Dictation is not installed")),
            OnboardingState::NoBackend => Some(gettextrs::gettext("No model installed")),
            OnboardingState::NoInputMethod => Some(gettextrs::gettext("No input method installed")),
            OnboardingState::NoModelConnected
            | OnboardingState::Ready
            | OnboardingState::Unavailable => None,
        };
        let setup_group = widget.setup_group();
        setup_group.set_visible(missing.is_some());
        if let Some(title) = missing {
            setup_group.set_title(&title);
        }

        widget.upcast()
    }

    fn on_diagnostics_page_shown(self: &Rc<Self>) {
        // Selecting Diagnostics performs an on-demand refresh.
        self.on_diagnostics_requested();
    }

    fn trigger_discovery(self: &Rc<Self>) {
        if self.controller.repository().is_none() {
            return;
        }
        let request: DiscoveryRequest = self.controller.begin_discovery();
        self.run_discovery(request);
    }

    /// A quiet (focus) discovery skips the clock probe and the Diagnostics
    /// snapshots, and redraws Diagnostics only when the inventory changed.
    fn run_discovery(self: &Rc<Self>, request: DiscoveryRequest) {
        let Some(repository) = self.controller.repository().cloned() else {
            return;
        };
        let quiet = request.quiet();
        let token = request.token();
        let inventory_token = token.clone();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // The probe loads a core for a fraction of a second; it runs on
            // the blocking pool alongside the snapd reads, not on this thread.
            let probe =
                (!quiet).then(|| gio::spawn_blocking(crate::performance::performance_facts));
            let inventory = repository.installed_snaps(inventory_token).await;
            let result = repository.refresh(token).await;
            let performance = match probe {
                Some(probe) => probe.await.ok(),
                None => None,
            };
            if let Some(ui) = ui.upgrade() {
                let accepted = ui.controller.complete_discovery(request, result);
                if !accepted {
                    return;
                }
                // Started after snapd was done, so what it found stands.
                ui.model_installs
                    .borrow_mut()
                    .retain(|_, install| *install != ModelInstall::Confirming);
                ui.render_installs();
                if performance.is_some() {
                    *ui.performance.borrow_mut() = performance;
                }
                let (snaps, failure) = match inventory {
                    Ok(snaps) => (snaps, None),
                    Err(error) => (Vec::new(), Some(problem_from_surface_error(&error))),
                };
                let changed = *ui.installed_snaps.borrow() != snaps
                    || *ui.inventory_failure.borrow() != failure;
                *ui.installed_snaps.borrow_mut() = snaps;
                *ui.inventory_failure.borrow_mut() = failure;
                ui.inventory_complete.set(true);
                if quiet && !changed {
                    return;
                }
                ui.rebuild_diagnostics_page();
                if !quiet && ui.view_stack.visible_child_name().as_deref() == Some("diagnostics") {
                    for page in ui.controller.pages() {
                        if !page.loading() {
                            ui.trigger_snapshot(page.identity().snap_name());
                        }
                    }
                }
            }
        });
    }

    fn trigger_snapshot(self: &Rc<Self>, snap_name: &str) {
        let Some(repository) = self.controller.repository().cloned() else {
            return;
        };
        let Some(request) = self.controller.begin_snapshot(snap_name) else {
            return;
        };
        let token = request.token();
        let Some(identity) = self
            .controller
            .page(snap_name)
            .map(|page| page.identity().clone())
        else {
            return;
        };
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let snapshot = repository.read_snapshot(&identity, token.clone()).await;
            if token.is_cancelled() {
                return;
            }
            if let Some(ui) = ui.upgrade() {
                ui.controller.complete_snapshot(request, snapshot);
            }
        });
    }

    fn on_diagnostics_requested(self: &Rc<Self>) {
        let now = Instant::now();
        if self
            .last_diagnostics_refresh
            .get()
            .is_some_and(|last| now.saturating_duration_since(last) < REFRESH_DEBOUNCE)
        {
            return;
        }
        self.last_diagnostics_refresh.set(Some(now));
        if !self.controller.discovery_loading() {
            self.trigger_discovery();
        }
    }

    pub fn shutdown(&self) {
        // Cancel any in-flight operations. There is no periodic refresh timer
        // to tear down — the UI is fully event-driven.
        abandon_all_apply_state(
            &mut self.apply_state.borrow_mut(),
            &self.operation_coordinator,
        );
        self.active_backend.abandon();
        self.controller.cancel_all();
        self.install_cancellation.cancel();
    }
}

/// What the error report says about a change that did not take.
struct ApplyNotice {
    /// Failed, rather than not confirmed either way.
    failed: bool,
    summary: String,
    details: String,
}

impl ApplyNotice {
    fn failed(summary: String, details: String) -> Option<Self> {
        Some(Self {
            failed: true,
            summary,
            details,
        })
    }

    fn unconfirmed(summary: String, details: String) -> Option<Self> {
        Some(Self {
            failed: false,
            summary,
            details,
        })
    }
}

/// The state an apply leaves to show, when it read one back, and what to
/// report about it: nothing for a success or a dismissed prompt.
fn apply_report(
    result: Result<ApplySuccess, ApplyFailure>,
) -> (Option<crate::domain::BackendSnapshot>, Option<ApplyNotice>) {
    match result {
        Ok(success) => (Some(success.snapshot().clone()), None),
        Err(ApplyFailure::CancelledExecution) => (None, None),
        Err(ApplyFailure::ReadBackMismatch {
            snapshot,
            mismatches,
        }) => (
            Some(*snapshot),
            ApplyNotice::failed(
                gettextrs::gettext("The model kept a different value than the one you chose."),
                diagnostics::redact_text(&mismatch_summary(&mismatches)),
            ),
        ),
        Err(ApplyFailure::RestartReadiness { snapshot, message }) => (
            Some(*snapshot),
            ApplyNotice::failed(
                gettextrs::gettext("The setting was saved, but the model did not start again."),
                diagnostics::redact_text(&message),
            ),
        ),
        Err(ApplyFailure::ReadBackUnavailable { snapshot, errors }) => (
            Some(*snapshot),
            ApplyNotice::unconfirmed(
                gettextrs::gettext("The setting was changed, but it could not be read back."),
                read_back_failure_details(&errors),
            ),
        ),
        Err(ApplyFailure::VerificationCancelled { snapshot, .. }) => (
            Some(*snapshot),
            ApplyNotice::unconfirmed(
                gettextrs::gettext("The setting was changed, but reading it back was interrupted."),
                String::new(),
            ),
        ),
        Err(ApplyFailure::PartialExecution {
            snapshot,
            commands,
            failure,
        }) => {
            let details = partial_execution_details(&failure, &commands, &snapshot);
            (
                Some(*snapshot),
                ApplyNotice::failed(system_failure_summary(&failure), details),
            )
        }
        Err(ApplyFailure::AuthorizationDenied { details }) => (
            None,
            ApplyNotice::failed(
                gettextrs::gettext("The system did not allow the change."),
                privileged_failure_details(&details),
            ),
        ),
        Err(ApplyFailure::ValuesRejected { details }) => (
            None,
            ApplyNotice::failed(
                gettextrs::gettext("The model rejected the value."),
                privileged_failure_details(&details),
            ),
        ),
        Err(ApplyFailure::Execution { details }) => (
            None,
            ApplyNotice::failed(
                gettextrs::gettext("The change could not be made."),
                privileged_failure_details(&details),
            ),
        ),
    }
}

pub(crate) fn system_failure_summary(error: &crate::ports::SystemConfiguratorError) -> String {
    match error {
        crate::ports::SystemConfiguratorError::Cancelled => {
            gettextrs::gettext("The change was interrupted after it started.")
        }
        crate::ports::SystemConfiguratorError::AuthorizationDenied { .. } => {
            gettextrs::gettext("The system did not allow the change.")
        }
        crate::ports::SystemConfiguratorError::ValuesRejected { .. } => {
            gettextrs::gettext("The model rejected the value.")
        }
        crate::ports::SystemConfiguratorError::Execution { .. } => {
            gettextrs::gettext("The change could not be made.")
        }
    }
}

fn privileged_failure_details(details: &crate::backend_apply::PrivilegedFailure) -> String {
    failure_details(Some(details.step()), details.message())
}

pub(crate) fn system_error_details(error: &crate::ports::SystemConfiguratorError) -> String {
    let message = match error {
        crate::ports::SystemConfiguratorError::Cancelled => {
            "privileged configuration was cancelled"
        }
        crate::ports::SystemConfiguratorError::AuthorizationDenied { message, .. }
        | crate::ports::SystemConfiguratorError::ValuesRejected { message, .. }
        | crate::ports::SystemConfiguratorError::Execution { message, .. } => message,
    };
    failure_details(error.step(), message)
}

/// Name the failed step as what it was, a command, a snapd request or a
/// D-Bus call, then the message. Every field but the request and the call
/// flows through [`diagnostics::redact_text`].
fn failure_details(step: Option<&crate::ports::FailedStep>, message: &str) -> String {
    let mut out = String::new();
    match step {
        Some(crate::ports::FailedStep::Command {
            executable,
            arguments,
            exit_status,
            stderr,
        }) => {
            out.push_str(&gettextrs::gettext("Executable:"));
            out.push(' ');
            out.push_str(&diagnostics::redact_text(executable));
            out.push('\n');
            out.push_str(&gettextrs::gettext("Arguments:"));
            if arguments.is_empty() {
                out.push_str(" -");
            }
            for argument in arguments {
                out.push(' ');
                out.push_str(&diagnostics::redact_text(argument));
            }
            out.push('\n');
            out.push_str(&gettextrs::gettext("Exit status:"));
            out.push(' ');
            match exit_status {
                Some(code) => out.push_str(&code.to_string()),
                None => out.push('-'),
            }
            out.push('\n');
            if !stderr.trim().is_empty() && stderr.trim() != message.trim() {
                out.push_str(&gettextrs::gettext("Standard error:"));
                out.push('\n');
                out.push_str(&diagnostics::redact_text(stderr));
                out.push('\n');
            }
        }
        Some(crate::ports::FailedStep::Snapd {
            request,
            http_status,
        }) => {
            out.push_str(&gettextrs::gettext("Request:"));
            out.push(' ');
            // Built from API paths and snap names: nothing private.
            out.push_str(request);
            out.push('\n');
            if let Some(status) = http_status {
                out.push_str(&gettextrs::gettext("HTTP status:"));
                out.push(' ');
                out.push_str(&status.to_string());
                out.push('\n');
            }
        }
        Some(crate::ports::FailedStep::DBus { call }) => {
            out.push_str(&gettextrs::gettext("D-Bus call:"));
            out.push(' ');
            out.push_str(call);
            out.push('\n');
        }
        Some(crate::ports::FailedStep::Setting { key }) => {
            out.push_str(&gettextrs::gettext("Setting:"));
            out.push(' ');
            out.push_str(key);
            out.push('\n');
        }
        None => {}
    }
    let message = diagnostics::redact_text(message);
    out.push_str(&gettextrs::gettext("Message:"));
    out.push(if message.contains('\n') { '\n' } else { ' ' });
    out.push_str(&message);
    out
}

/// Redact-and-format the full details dialog body for a partial-execution
/// apply failure. Includes the completed commands (executable + argv + exit
/// status + stderr), the underlying error, and the reconciliation snapshot
/// from the post-failure read-back.
fn partial_execution_details(
    error: &crate::ports::SystemConfiguratorError,
    commands: &[crate::domain::CommandResult],
    snapshot: &crate::domain::BackendSnapshot,
) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext(
        "Some operations completed before the apply failed.\n",
    ));
    out.push('\n');
    if commands.is_empty() {
        out.push_str(&gettextrs::gettext("No privileged operations completed.\n"));
    } else {
        out.push_str(&gettextrs::gettext("Completed operations:\n"));
        for result in commands {
            out.push_str("  ");
            out.push_str(&diagnostics::redact_text(result.executable()));
            for arg in result.arguments() {
                out.push(' ');
                out.push_str(&diagnostics::redact_text(arg));
            }
            out.push_str(&format!(
                "\n    {} {}\n",
                gettextrs::gettext("Exit status:"),
                result
                    .exit_status()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
            ));
            if !result.stderr().trim().is_empty() {
                out.push_str(&format!(
                    "    {}\n    {}\n",
                    gettextrs::gettext("Standard error:"),
                    diagnostics::redact_text(result.stderr()).replace('\n', "\n    "),
                ));
            }
        }
    }
    out.push('\n');
    out.push_str(&gettextrs::gettext("Failure:\n"));
    out.push_str(&system_error_details(error));
    out.push('\n');
    out.push('\n');
    out.push_str(&gettextrs::gettext("Reconciliation read-back:\n"));
    out.push_str(&backend_snapshot_reconciliation_summary(snapshot));
    out
}

/// Compact per-snapshot reconciliation summary: service state, entrypoints,
/// and the surface errors that survived post-failure discovery. Every value
/// is redacted with [`diagnostics::redact_text`].
fn backend_snapshot_reconciliation_summary(snapshot: &crate::domain::BackendSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {}\n",
        gettextrs::gettext("Model:"),
        diagnostics::redact_text(snapshot.identity().snap_name()),
    ));
    if let Some(status) = snapshot.status() {
        let services = status.services();
        if services.is_empty() {
            out.push_str(&format!("  {}\n", gettextrs::gettext("Services: none"),));
        } else {
            out.push_str(&format!("  {}\n", gettextrs::gettext("Services:")));
            for service in services {
                out.push_str(&format!(
                    "    {} {}\n",
                    diagnostics::redact_text(service.name()),
                    diagnostics::redact_text(&service_state_label(service.state())),
                ));
            }
        }
    } else {
        out.push_str(&format!(
            "  {}\n",
            gettextrs::gettext("Status: unavailable"),
        ));
    }
    let errors = snapshot.errors();
    if !errors.is_empty() {
        out.push_str(&format!(
            "  {}\n",
            gettextrs::gettext("Post-failure surface errors:"),
        ));
        for (surface, error) in errors {
            out.push_str(&format!(
                "    {}: {}\n",
                diagnostic_surface_label(*surface),
                diagnostics::redact_text(error.message()),
            ));
        }
    }
    out
}

impl Drop for BackendUi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn display_title_for(snap_name: &str) -> String {
    model_family(snap_name).name
}

fn setting_widget_name(key: &str) -> String {
    format!("myna-setting-{}", key.replace('.', "-"))
}

/// The Model tab's group for the spoken-language row, which General's
/// settings page fills.
pub(crate) fn spoken_language_group() -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Language"))
        .description(gettextrs::gettext(
            "A code such as en, fr or zh. Leave it empty to detect the language.",
        ))
        .build()
}

/// Widget name of a row General builds from GSettings and the Model tab
/// shows, so a page rebuild hands focus back to it.
pub(crate) fn client_setting_widget_name(key: &str) -> String {
    setting_widget_name(&format!("client-{key}"))
}

const APPLY_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Widget name of the progress line an entry row shows while it changes.
const APPLY_PROGRESS: &str = "myna-apply-progress";

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        found.extend(descendants(&widget));
        found.push(widget);
    }
    found
}

fn find_named_descendant(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_named_descendant(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn model_status_page(title: &str, description: &str, icon: &str) -> adw::NavigationPage {
    let page = ui::StatusPage::new();
    page.set_status(title, description, icon);
    page.upcast()
}

/// Fills `page_widget` with `page`'s groups, replacing what it showed.
fn populate_backend_page(page_widget: &ui::BackendPage, page: &BackendPage, ui: &Rc<BackendUi>) {
    let mut groups = Vec::new();
    let title = display_title_for(page.identity().snap_name());
    page_widget.set_display_title(&title);

    let overview = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Overview"))
        .description(escape_markup(&overview_description(page)))
        .build();
    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .valign(gtk::Align::Center)
        .build();
    overview.set_header_suffix(Some(&refresh));
    let short = page.short_status();
    let health = adw::ActionRow::builder()
        .title(gettextrs::gettext("Connection"))
        .subtitle(escape_markup(&match short.connection {
            ConnectionKind::Active => {
                gettextrs::gettext("Active. Myna uses this model for dictation.")
            }
            ConnectionKind::Contested => {
                gettextrs::gettext("Several models are connected. Only one should be active.")
            }
            ConnectionKind::Disconnected => gettextrs::gettext("Not connected to Myna."),
        }))
        .build();
    overview.add(&health);
    if let Some(model) = &short.active_model {
        let model_row = adw::ActionRow::builder()
            .title(gettextrs::gettext("Active model"))
            .subtitle(escape_markup(model))
            .build();
        overview.add(&model_row);
    }
    let engine_row = adw::ActionRow::builder()
        .title(gettextrs::gettext("Active engine"))
        .subtitle(escape_markup(&match &short.active_engine {
            Some(engine) => engine.clone(),
            None => gettextrs::gettext("None selected. Choose one below."),
        }))
        .build();
    overview.add(&engine_row);
    if let Some(status) = page.snapshot().and_then(|snapshot| snapshot.status()) {
        let services = status
            .services()
            .iter()
            .map(|service| {
                format!(
                    "{}: {}",
                    service.name(),
                    service_state_label(service.state())
                )
            })
            .collect::<Vec<_>>();
        let service_row = adw::ActionRow::builder()
            .title(gettextrs::gettext("Services"))
            .subtitle(escape_markup(&if services.is_empty() {
                gettextrs::gettext("No service health reported")
            } else {
                services.join(" · ")
            }))
            .build();
        overview.add(&service_row);
    }
    groups.push(overview);
    if ui.shown_backend().as_deref() == Some(page.identity().snap_name()) {
        groups.extend(ui.spoken_language_for(page.identity().snap_name()));
    }

    if page.loading() && page.snapshot().is_none() {
        let loading = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Loading"))
            .description(gettextrs::gettext("Reading this model's settings…"))
            .build();
        groups.push(loading);
    } else if let Some(snapshot) = page.snapshot() {
        if snapshot.configuration().is_empty()
            && snapshot.models().is_none()
            && snapshot.engines().is_none()
        {
            let empty = adw::PreferencesGroup::builder()
                .title(gettextrs::gettext("No configuration reported"))
                .description(gettextrs::gettext(
                    "The model answered but reported no settings to change.",
                ))
                .build();
            groups.push(empty);
        }
        groups.extend(configuration_groups(page, ui));
    }

    if page.partial() {
        let details = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Some data is unavailable"))
            .description(gettextrs::gettext(
                "Raw failure details are recorded on the Diagnostics page.",
            ))
            .build();
        groups.push(details);
    }

    let applying = ui.apply_state_view(page.identity().snap_name()).is_some();
    let (refresh_enabled, refresh_label) = refresh_control_state(page.loading(), applying);
    refresh.set_icon_name(if page.loading() || applying {
        "content-loading-symbolic"
    } else {
        "view-refresh-symbolic"
    });
    refresh.set_tooltip_text(Some(&refresh_label));
    refresh.set_sensitive(refresh_enabled);
    refresh.update_property(&[gtk::accessible::Property::Label(&refresh_label)]);
    let actions = gio::SimpleActionGroup::new();
    let refresh_action = gio::SimpleAction::new("refresh", None);
    refresh_action.set_enabled(refresh_enabled);
    let snap_name = page.identity().snap_name().to_owned();
    refresh_action.connect_activate({
        let controller = Rc::downgrade(&ui.controller);
        let snap = snap_name.clone();
        move |_, _| {
            if let Some(controller) = controller.upgrade() {
                trigger_manual_refresh(&controller, &snap);
            }
        }
    });
    actions.add_action(&refresh_action);
    page_widget.insert_action_group("backend", Some(&actions));
    refresh.set_action_name(Some("backend.refresh"));
    page_widget.set_groups(groups);
}

fn overview_description(page: &BackendPage) -> String {
    let mut description = format!(
        "{} ({}).",
        display_title_for(page.identity().snap_name()),
        page.identity().snap_name()
    );
    if page.loading() {
        description.push(' ');
        description.push_str(&gettextrs::gettext("Refreshing…"));
    }
    description
}

fn configuration_groups(page: &BackendPage, ui: &Rc<BackendUi>) -> Vec<adw::PreferencesGroup> {
    let applying = ui.apply_state_view(page.identity().snap_name());
    let mut groups: BTreeMap<crate::presentation::PresentationGroup, adw::PreferencesGroup> =
        BTreeMap::new();
    for row in page.rows() {
        if row.metadata().diagnostics_only() {
            continue;
        }
        let key = row.metadata().group();
        let group = groups.entry(key).or_insert_with(|| {
            adw::PreferencesGroup::builder()
                .title(group_title(key))
                .description(group_description(key))
                .build()
        });
        group.add(&build_row_widget(row, ui, page, applying.as_ref()));
    }
    groups.into_values().collect()
}

fn group_title(group: crate::presentation::PresentationGroup) -> String {
    match group {
        crate::presentation::PresentationGroup::General => gettextrs::gettext("General"),
        crate::presentation::PresentationGroup::Runtime => gettextrs::gettext("Runtime"),
        crate::presentation::PresentationGroup::Advanced => gettextrs::gettext("Advanced"),
        crate::presentation::PresentationGroup::Sensitive => gettextrs::gettext("Sensitive"),
    }
}

fn group_description(group: crate::presentation::PresentationGroup) -> String {
    match group {
        crate::presentation::PresentationGroup::General => {
            gettextrs::gettext("Common settings for this model.")
        }
        crate::presentation::PresentationGroup::Runtime => {
            gettextrs::gettext("Runtime and performance related settings.")
        }
        crate::presentation::PresentationGroup::Advanced => {
            gettextrs::gettext("Advanced settings.")
        }
        crate::presentation::PresentationGroup::Sensitive => {
            gettextrs::gettext("Internal and sensitive values are shown for diagnostics only.")
        }
    }
}

fn build_row_widget(
    row: &PresentationRow,
    ui: &Rc<BackendUi>,
    page: &BackendPage,
    applying: Option<&ApplyingView>,
) -> gtk::Widget {
    let snap = page.identity().snap_name().to_owned();
    let key = row.key().to_owned();
    let metadata = row.metadata();
    let title = metadata.title();
    let description = metadata.explanation();
    let editable = applying.is_none();
    // The row being changed shows the value it is changing to and what the
    // change is doing.
    let changing = applying.filter(|applying| applying.key == key);
    let value = changing.map_or(row.value(), |changing| &changing.value);
    let progress = changing.map(|changing| changing.progress.as_str());
    if metadata.diagnostics_only() {
        let value = if metadata.sensitivity() == Sensitivity::Sensitive {
            gettextrs::gettext("Sensitive value (redacted)")
        } else {
            diagnostics::redact_text(&config_value_display(value))
        };
        let action = adw::ActionRow::builder()
            .title(title)
            .subtitle(escape_markup(&value))
            .build();
        action.set_activatable(false);
        action.set_sensitive(editable);
        action
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Description(description)]);
        return action.upcast();
    }
    let changed = {
        let ui = Rc::downgrade(ui);
        move |value: ConfigValue| {
            if let Some(ui) = ui.upgrade() {
                ui.request_change(&snap, &key, value);
            }
        }
    };
    match metadata.control() {
        ControlType::Toggle => {
            let switch = adw::SwitchRow::builder()
                .title(title)
                .subtitle(description)
                .active(matches!(value, ConfigValue::Boolean(true)))
                .build();
            switch.set_widget_name(&setting_widget_name(row.key()));
            switch
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            switch.set_sensitive(editable);
            show_row_progress(switch.upcast_ref(), progress);
            switch.connect_active_notify(move |row| changed(ConfigValue::Boolean(row.is_active())));
            switch.upcast()
        }
        ControlType::Choice => {
            let choices = row.choices();
            let model =
                gtk::StringList::new(&choices.iter().map(String::as_str).collect::<Vec<_>>());
            let combo = adw::ComboRow::builder()
                .title(title)
                .subtitle(description)
                .model(&model)
                .build();
            combo.set_widget_name(&setting_widget_name(row.key()));
            combo
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            combo.set_sensitive(editable);
            show_row_progress(combo.upcast_ref(), progress);
            if let ConfigValue::Text(current) = value {
                if let Some(index) = choices.iter().position(|choice| choice == current) {
                    combo.set_selected(index as u32);
                }
            }
            let choices = choices.to_vec();
            combo.connect_selected_notify(move |row| {
                if let Some(choice) = choices.get(row.selected() as usize) {
                    changed(ConfigValue::Text(choice.clone()));
                }
            });
            combo.upcast()
        }
        ControlType::ReadOnly => {
            let action = adw::ActionRow::builder()
                .title(title)
                .subtitle(escape_markup(&config_value_display(value)))
                .build();
            action.set_activatable(false);
            action.set_sensitive(editable);
            action
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            action.upcast()
        }
        ControlType::Number | ControlType::Text => {
            let entry = adw::EntryRow::builder()
                .title(title)
                .text(config_value_display(value))
                .show_apply_button(true)
                .build();
            entry.set_widget_name(&setting_widget_name(row.key()));
            entry.set_tooltip_text(Some(description));
            entry
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            entry.set_sensitive(editable);
            if let Some(progress) = progress {
                // An entry row has no subtitle: the line goes at its end.
                let label = gtk::Label::builder()
                    .label(progress)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["dim-label"])
                    .build();
                label.set_widget_name(APPLY_PROGRESS);
                entry.add_suffix(&label);
                entry.add_prefix(&progress_spinner());
                hold_while_changing(entry.upcast_ref());
                // Its pencil would say the row can be edited.
                for icon in descendants(entry.upcast_ref()) {
                    if icon.has_css_class("edit-icon") {
                        icon.set_visible(false);
                    }
                }
            }
            let control = metadata.control();
            // Applies on Enter or the apply button, never per keystroke.
            entry.connect_apply(move |row| {
                changed(parse_editable_value(control, row.text().as_str()));
            });
            entry.upcast()
        }
    }
}

/// Shows the change `row` is making in its subtitle.
fn show_row_progress(row: &adw::ActionRow, progress: Option<&str>) {
    let Some(progress) = progress else {
        return;
    };
    row.set_subtitle(&escape_markup(progress));
    row.add_prefix(&progress_spinner());
    hold_while_changing(row.upcast_ref());
}

fn progress_spinner() -> gtk::Spinner {
    let spinner = gtk::Spinner::new();
    spinner.start();
    spinner
}

/// The changing row stays sensitive but takes no input: insensitive, it
/// would dim its progress line with the other rows.
fn hold_while_changing(row: &gtk::Widget) {
    row.set_sensitive(true);
    row.set_can_target(false);
    row.set_can_focus(false);
    row.update_state(&[gtk::accessible::State::Busy(true)]);
}

pub(crate) fn apply_progress_text(progress: &ApplyProgress) -> String {
    match progress {
        ApplyProgress::Download { name, done, total } => {
            // TRANSLATORS: {name} is the model part or snap being downloaded, such as "model-small"; {done} and {total} are sizes such as "210.0 MB".
            let frame = gettextrs::gettext("Downloading {name}: {done} of {total}");
            frame
                .replace("{done}", &glib::format_size(*done))
                .replace("{total}", &glib::format_size(*total))
                .replace("{name}", name)
        }
        ApplyProgress::Change { summary } => summary.clone(),
    }
}

fn service_state_label(state: &ServiceState) -> String {
    match state {
        ServiceState::Active => gettextrs::gettext("Active"),
        ServiceState::Inactive => gettextrs::gettext("Inactive"),
        ServiceState::Failed => gettextrs::gettext("Failed"),
        ServiceState::Unknown(value) => {
            format!("{} ({value})", gettextrs::gettext("Unknown"))
        }
    }
}

fn backend_diagnostic_from(page: &BackendPage, snaps: &[InstalledSnap]) -> BackendDiagnostic {
    let snap_name = page.identity().snap_name().to_owned();
    let short = page.short_status();
    let snapshot = page.snapshot();
    BackendDiagnostic {
        version: snaps
            .iter()
            .find(|snap| snap.name == snap_name)
            .map(|snap| snap.version.clone())
            .unwrap_or_default(),
        memory: crate::machine::snap_process(&snap_name),
        snap_name,
        connection: match page.connection() {
            ConnectionKind::Active => DiagnosticConnection::Connected,
            ConnectionKind::Contested => DiagnosticConnection::MultipleConnections,
            ConnectionKind::Disconnected => DiagnosticConnection::NotConnected,
        },
        engine: short.active_engine.clone(),
        model: short.active_model.clone(),
        services: snapshot
            .map(|snapshot| {
                snapshot
                    .status()
                    .map(|status| {
                        status
                            .services()
                            .iter()
                            .map(|service| {
                                format!(
                                    "{}: {}",
                                    service.name(),
                                    service_state_label(service.state())
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
        problems: page
            .errors()
            .iter()
            .map(problem_from_surface_error)
            .collect(),
    }
}

/// One sentence naming the surface and what it said. Deliberately not the
/// command line: the report is pasted into bug reports.
fn problem_from_surface_error(error: &crate::domain::BackendSurfaceError) -> String {
    format!(
        "{}: {}",
        diagnostic_surface_label(error.surface()),
        diagnostics::redact_text(error.message())
    )
}

fn diagnostic_surface_label(surface: crate::domain::BackendSurface) -> String {
    use crate::domain::BackendSurface;
    match surface {
        BackendSurface::SnapInventory => gettextrs::gettext("Installed snaps"),
        BackendSurface::Connections => gettextrs::gettext("Model connections"),
        BackendSurface::ModelctlApp => gettextrs::gettext("Model control command"),
        BackendSurface::ModelctlConfig => gettextrs::gettext("Model settings"),
        BackendSurface::Status => gettextrs::gettext("Model status"),
        BackendSurface::Models => gettextrs::gettext("Available models"),
        BackendSurface::Engines => gettextrs::gettext("Available engines"),
    }
}

fn read_back_failure_details(errors: &[crate::domain::BackendSurfaceError]) -> String {
    if errors.is_empty() {
        return gettextrs::gettext("Persisted values could not be read.");
    }
    errors
        .iter()
        .map(problem_from_surface_error)
        .collect::<Vec<_>>()
        .join("\n")
}

fn refresh_control_state(loading: bool, applying: bool) -> (bool, String) {
    if applying {
        (false, gettextrs::gettext("Applying changes…"))
    } else if loading {
        (false, gettextrs::gettext("Refreshing…"))
    } else {
        (true, gettextrs::gettext("Refresh"))
    }
}

fn validation_issue_summary(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|issue| format!("{}: {}", issue.title(), issue.message()))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn mismatch_summary(mismatches: &[crate::backend_apply::ReadBackMismatch]) -> String {
    mismatches
        .iter()
        .map(|mismatch| {
            let actual = mismatch
                .actual()
                .map(config_value_display)
                .unwrap_or_else(|| gettextrs::gettext("missing"));
            // TRANSLATORS: {key} is a setting's key, such as "streaming"; {requested} and {actual} are its values.
            let frame = gettextrs::gettext("{key}: asked for {requested}, read back {actual}");
            frame
                .replace("{key}", mismatch.key())
                .replace("{requested}", &config_value_display(mismatch.requested()))
                .replace("{actual}", &actual)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A one-line reason and the copyable report for a switch that did not take.
/// Everything goes through [`diagnostics::redact_text`], so neither the home
/// directory nor a secret-looking value reaches the dialog.
fn switch_report(outcome: &SwitchOutcome) -> (String, String) {
    let mut details = String::new();
    let summary = match outcome {
        SwitchOutcome::Failed {
            error,
            discovery_error,
            completed,
            final_snapshot,
        } => {
            push_final_connections(
                &mut details,
                final_snapshot.as_ref(),
                discovery_error.as_ref(),
            );
            push_completed_operations(&mut details, completed);
            details.push_str(&gettextrs::gettext("Error:\n"));
            details.push_str(&system_error_details(error));
            gettextrs::gettext("The change could not be made.")
        }
        SwitchOutcome::Disagreed {
            completed,
            final_snapshot,
        } => {
            push_final_connections(&mut details, Some(final_snapshot), None);
            push_completed_operations(&mut details, completed);
            gettextrs::gettext("The change ran, but the connections do not match it.")
        }
        SwitchOutcome::StaleDiscovery { final_snapshot } => {
            push_final_connections(&mut details, Some(final_snapshot), None);
            gettextrs::gettext("The connections changed before the change could start. Try again.")
        }
        SwitchOutcome::FinalDiscoveryFailed { error, completed } => {
            push_completed_operations(&mut details, completed);
            details.push_str(&gettextrs::gettext("Discovery error:\n"));
            details.push_str(&diagnostics::redact_text(error.message()));
            gettextrs::gettext("The change ran, but the connections could not be read back.")
        }
        SwitchOutcome::Applied { .. }
        | SwitchOutcome::Noop { .. }
        | SwitchOutcome::Cancelled { .. } => String::new(),
    };
    (summary, details.trim_end().to_owned())
}

fn push_final_connections(
    out: &mut String,
    final_snapshot: Option<&crate::domain::ConnectionSnapshot>,
    discovery_error: Option<&crate::domain::BackendSurfaceError>,
) {
    match (final_snapshot, discovery_error) {
        (Some(snapshot), _) => out.push_str(&format!(
            "{} {}\n",
            gettextrs::gettext("Final connections:"),
            connection_state_summary(snapshot)
        )),
        (None, Some(discovery_error)) => out.push_str(&format!(
            "{} {}\n",
            gettextrs::gettext("Final connections could not be verified:"),
            diagnostics::redact_text(discovery_error.message())
        )),
        (None, None) => out.push_str(&gettextrs::gettext(
            "Final connections could not be verified.\n",
        )),
    }
    out.push('\n');
}

fn push_completed_operations(out: &mut String, completed: &[crate::domain::CommandResult]) {
    if completed.is_empty() {
        out.push_str(&gettextrs::gettext("No snapd operations completed.\n"));
        out.push('\n');
        return;
    }
    out.push_str(&gettextrs::gettext("Completed snapd operations:\n"));
    for result in completed {
        out.push_str("  ");
        out.push_str(&diagnostics::redact_text(result.executable()));
        for arg in result.arguments() {
            out.push(' ');
            out.push_str(&diagnostics::redact_text(arg));
        }
        out.push_str(&format!(
            "\n    {} {}\n",
            gettextrs::gettext("Exit status:"),
            result
                .exit_status()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_owned()),
        ));
        if !result.stderr().trim().is_empty() {
            out.push_str(&format!(
                "    {}\n    {}\n",
                gettextrs::gettext("Standard error:"),
                diagnostics::redact_text(result.stderr()).replace('\n', "\n    "),
            ));
        }
    }
    out.push('\n');
}

fn connection_state_summary(snapshot: &crate::domain::ConnectionSnapshot) -> String {
    match snapshot.active_state() {
        crate::domain::ActiveBackendState::Disconnected => gettextrs::gettext("no model connected"),
        crate::domain::ActiveBackendState::Connected(backend) => {
            format!(
                "{}: {}",
                gettextrs::gettext("connected"),
                diagnostics::redact_text(backend.snap_name())
            )
        }
        crate::domain::ActiveBackendState::MultiplyConnected(backends) => {
            let names = backends
                .iter()
                .map(|b| diagnostics::redact_text(b.snap_name()))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}: {}", gettextrs::gettext("several models"), names)
        }
    }
}

fn parse_editable_value(control: ControlType, raw: &str) -> ConfigValue {
    if control == ControlType::Number {
        if let Ok(integer) = raw.trim().parse::<i64>() {
            return ConfigValue::Integer(integer);
        }
        if let Ok(number) = raw.trim().parse::<f64>() {
            return ConfigValue::Number(number);
        }
    }
    ConfigValue::Text(raw.to_owned())
}

fn config_value_display(value: &ConfigValue) -> String {
    match value {
        ConfigValue::Null => String::new(),
        ConfigValue::Boolean(true) => "true".to_owned(),
        ConfigValue::Boolean(false) => "false".to_owned(),
        ConfigValue::Integer(number) => number.to_string(),
        ConfigValue::Number(number) => number.to_string(),
        ConfigValue::Text(text) => text.clone(),
    }
}

fn trigger_manual_refresh(controller: &Rc<BackendController>, snap_name: &str) {
    let Some(repository) = controller.repository().cloned() else {
        return;
    };
    let Some(request) = controller.begin_snapshot(snap_name) else {
        return;
    };
    let token = request.token();
    let Some(identity) = controller
        .page(snap_name)
        .map(|page| page.identity().clone())
    else {
        return;
    };
    let controller_weak = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        let snapshot = repository.read_snapshot(&identity, token.clone()).await;
        if token.is_cancelled() {
            return;
        }
        if let Some(controller) = controller_weak.upgrade() {
            controller.complete_snapshot(request, snapshot);
        }
    });
}

/// `win.setup` is off while its wizard is open.
struct SetupGate {
    action: gio::SimpleAction,
}

impl SetupGate {
    fn new(action: gio::SimpleAction) -> Self {
        Self { action }
    }

    fn set_wizard_open(&self, open: bool) {
        self.action.set_enabled(!open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn setup_is_off_while_its_wizard_is_open() {
        let action = gio::SimpleAction::new("setup", None);
        let gate = SetupGate::new(action.clone());
        gate.set_wizard_open(true);
        assert!(!action.is_enabled(), "the wizard is open");
        gate.set_wizard_open(false);
        assert!(action.is_enabled());
    }

    #[test]
    fn parse_editable_value_prefers_integer_then_number_then_text() {
        assert_eq!(
            parse_editable_value(ControlType::Number, "42"),
            ConfigValue::Integer(42)
        );
        assert_eq!(
            parse_editable_value(ControlType::Number, "3.5"),
            ConfigValue::Number(3.5)
        );
        assert_eq!(
            parse_editable_value(ControlType::Text, "hello"),
            ConfigValue::Text("hello".to_owned())
        );
    }

    #[test]
    fn refresh_control_is_pending_and_disabled_while_loading() {
        assert_eq!(
            refresh_control_state(true, false),
            (false, gettextrs::gettext("Refreshing…"))
        );
        assert_eq!(
            refresh_control_state(false, false),
            (true, gettextrs::gettext("Refresh"))
        );
        assert_eq!(
            refresh_control_state(false, true),
            (false, gettextrs::gettext("Applying changes…"))
        );
    }

    struct TestUi {
        ui: Rc<BackendUi>,
        view_stack: adw::ViewStack,
    }

    fn test_ui(controller: Rc<BackendController>) -> TestUi {
        test_ui_with(controller, None)
    }

    fn test_ui_with(
        controller: Rc<BackendController>,
        myna_selector: Option<ui::MynaPage>,
    ) -> TestUi {
        test_ui_full(controller, myna_selector, None)
    }

    fn test_ui_full(
        controller: Rc<BackendController>,
        myna_selector: Option<ui::MynaPage>,
        spoken_language: Option<adw::PreferencesGroup>,
    ) -> TestUi {
        test_ui_ports(
            controller,
            myna_selector,
            spoken_language,
            Rc::new(PkexecSystemConfigurator::new(Arc::new(GioCommandRunner))),
        )
    }

    fn test_ui_ports(
        controller: Rc<BackendController>,
        myna_selector: Option<ui::MynaPage>,
        spoken_language: Option<adw::PreferencesGroup>,
        configurator: Rc<dyn SystemConfigurator>,
    ) -> TestUi {
        let view_stack = adw::ViewStack::new();
        let backend_nav = adw::NavigationView::new();
        let diagnostics_nav = adw::NavigationView::new();
        view_stack.add_named(&gtk::Label::new(None), Some("general"));
        view_stack.add_named(&backend_nav, Some("model"));
        view_stack.add_named(&diagnostics_nav, Some("diagnostics"));
        let page = |title: &str| {
            adw::NavigationPage::builder()
                .title(title)
                .child(&gtk::Label::new(Some(title)))
                .build()
        };
        let operation_coordinator = OperationCoordinator::new();
        let ui = Rc::new_cyclic(|this| BackendUi {
            this: this.clone(),
            model_rows: RefCell::new(Vec::new()),
            model_radio_group: gtk::CheckButton::new(),
            marking_models: std::cell::Cell::new(false),
            preferred_languages: RefCell::new(None),
            controller,
            configurator,
            view_stack: view_stack.clone(),
            backend_nav,
            diagnostics_nav,
            overlay: adw::ToastOverlay::new(),
            myna_selector,
            spoken_language,
            client_settings: None,
            active_backend: ActiveBackendController::with_coordinator(
                crate::domain::ConnectionSnapshot::new(
                    Vec::new(),
                    ActiveBackendState::Disconnected,
                ),
                operation_coordinator.clone(),
            ),
            operation_coordinator,
            diagnostics_page: RefCell::new(Some(page("Diagnostics"))),
            backend_pages: RefCell::new(BTreeMap::new()),
            apply_state: RefCell::new(BTreeMap::new()),
            shown_state: RefCell::new(None),
            backend_tab_shown: std::cell::Cell::new(
                view_stack.visible_child_name().as_deref() == Some("model"),
            ),
            installed_snaps: RefCell::new(Vec::new()),
            inventory_complete: std::cell::Cell::new(false),
            inventory_failure: RefCell::new(None),
            performance: RefCell::new(None),
            last_diagnostics_refresh: std::cell::Cell::new(None),
            install_dialog: RefCell::new(None),
            model_installs: RefCell::default(),
            install_cancellation: CancellationToken::new(),
            follow_interval: std::cell::Cell::new(std::time::Duration::from_millis(5)),
            nvidia_gpu: false,
        });
        ui.connect_view_stack_selection();
        ui.connect_install_button();
        TestUi { ui, view_stack }
    }

    use crate::ui::on_gtk_thread;

    #[test]
    fn switching_to_the_diagnostics_tab_triggers_a_refresh() {
        on_gtk_thread(|| {
            let TestUi { ui, view_stack } = test_ui(BackendController::detached());
            assert!(ui.last_diagnostics_refresh.get().is_none());
            view_stack.set_visible_child_name("diagnostics");
            assert!(ui.last_diagnostics_refresh.get().is_some());
        });
    }

    const PARAKEET_SLOT: &str =
        "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n";
    const PARAKEET_CONNECTED: &str = "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend myna-parakeet:provider manual\n";
    const PARAKEET_DISCONNECTED: &str =
        "Interface Plug Slot Notes\ncontent - myna-parakeet:provider -\n";
    const PARAKEET_AND_WHISPER_SLOTS: &str = "name: content\nslots:\n  \
         - myna-parakeet:provider:\n      content: inference-provider\n  \
         - myna-whisper:provider:\n      content: inference-provider\n";
    const PARAKEET_CONNECTED_WHISPER_INSTALLED: &str = "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend myna-parakeet:provider manual\n\
         content - myna-whisper:provider -\n";

    const WHISPER_CONNECTED_PARAKEET_INSTALLED: &str = "Interface Plug Slot Notes\n\
         content - myna-parakeet:provider -\n\
         content[inference-provider] myna:backend myna-whisper:provider manual\n";

    const NEITHER_CONNECTED: &str = "Interface Plug Slot Notes\n\
         content - myna-parakeet:provider -\n\
         content - myna-whisper:provider -\n";

    /// The Model tab over `connections`, with a spoken-language group holding
    /// one entry row, shown on screen.
    fn spoken_language_ui(connections: &str) -> (Rc<BackendUi>, adw::EntryRow, gtk::Window) {
        ui::register_resources();
        let row = adw::EntryRow::builder()
            .title("Spoken language")
            .name(client_setting_widget_name("language"))
            .build();
        let group = adw::PreferencesGroup::new();
        group.add(&row);
        let TestUi { ui, view_stack } = test_ui_full(
            discovered_with(connections, PARAKEET_AND_WHISPER_SLOTS),
            None,
            Some(group),
        );
        view_stack.set_visible_child_name("model");
        let window = gtk::Window::builder().child(&view_stack).build();
        window.present();
        ui.sync_active_backend();
        (ui, row, window)
    }

    fn on_model_tab(ui: &BackendUi, row: &adw::EntryRow) -> bool {
        ui.backend_nav
            .visible_page()
            .is_some_and(|page| row.is_ancestor(&page))
    }

    fn rediscover_as(ui: &Rc<BackendUi>, connections: &str) {
        let request = ui.controller.begin_discovery();
        let snapshot = crate::domain::parse_connections(connections, PARAKEET_AND_WHISPER_SLOTS)
            .expect("connections parse");
        ui.controller.complete_discovery(request, Ok(snapshot));
        ui.sync_active_backend();
    }

    #[test]
    fn the_spoken_language_shows_on_the_model_tab_only_for_whisper() {
        on_gtk_thread(|| {
            let (ui, row, _window) = spoken_language_ui(WHISPER_CONNECTED_PARAKEET_INSTALLED);
            assert!(on_model_tab(&ui, &row), "missing with Whisper active");

            rediscover_as(&ui, PARAKEET_CONNECTED_WHISPER_INSTALLED);
            assert!(!on_model_tab(&ui, &row), "shown with Parakeet active");
            assert!(row.root().is_none(), "left on a page with Parakeet active");

            rediscover_as(&ui, WHISPER_CONNECTED_PARAKEET_INSTALLED);
            assert!(on_model_tab(&ui, &row), "missing after switching back");

            rediscover_as(&ui, NEITHER_CONNECTED);
            assert!(row.root().is_none(), "left on a page with nothing active");
        });
    }

    #[test]
    fn a_rebuilt_whisper_page_keeps_the_spoken_language_focused() {
        on_gtk_thread(|| {
            let (ui, row, window) = spoken_language_ui(WHISPER_CONNECTED_PARAKEET_INSTALLED);
            row.set_text("fr");
            row.grab_focus();
            row.set_position(1);

            ui.rebuild_backend_page("myna-whisper");
            // GTK moves the focus off a detached widget on a later idle.
            while glib::MainContext::default().iteration(false) {}

            assert!(on_model_tab(&ui, &row));
            let focus = gtk::prelude::GtkWindowExt::focus(&window).expect("a focus widget");
            assert!(
                focus.has_focus() && focus.is_ancestor(&row),
                "focus left the row"
            );
            assert_eq!(row.text().as_str(), "fr");
            assert_eq!(row.position(), 1);
        });
    }

    /// Never answers, so a read the UI starts stays in flight.
    struct UnansweredRepository;

    #[async_trait::async_trait(?Send)]
    impl BackendRepository for UnansweredRepository {
        async fn discover(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            std::future::pending().await
        }

        async fn read_snapshot(
            &self,
            _backend: &BackendIdentity,
            _cancellation: CancellationToken,
        ) -> crate::domain::BackendSnapshot {
            std::future::pending().await
        }

        async fn refresh(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            std::future::pending().await
        }
    }

    fn discovered(connections: &str) -> Rc<BackendController> {
        discovered_with(connections, PARAKEET_SLOT)
    }

    fn discovered_with(connections: &str, slots: &str) -> Rc<BackendController> {
        let controller = BackendController::new(Rc::new(UnansweredRepository));
        let request = controller.begin_discovery();
        let snapshot =
            crate::domain::parse_connections(connections, slots).expect("connections parse");
        controller.complete_discovery(request, Ok(snapshot));
        controller
    }

    fn parakeet_loading(ui: &BackendUi) -> bool {
        ui.controller
            .page("myna-parakeet")
            .expect("parakeet discovered")
            .loading()
    }

    #[test]
    fn the_active_backend_is_read_only_once_its_tab_is_shown() {
        on_gtk_thread(|| {
            let TestUi { ui, view_stack } = test_ui(discovered(PARAKEET_CONNECTED));
            ui.sync_active_backend();
            assert!(!parakeet_loading(&ui));

            view_stack.set_visible_child_name("model");
            assert!(parakeet_loading(&ui));
        });
    }

    #[test]
    fn leaving_the_backend_tab_cancels_its_read() {
        on_gtk_thread(|| {
            let TestUi { ui, view_stack } = test_ui(discovered(PARAKEET_CONNECTED));
            ui.sync_active_backend();
            view_stack.set_visible_child_name("model");
            assert!(parakeet_loading(&ui));

            view_stack.set_visible_child_name("general");
            assert!(!parakeet_loading(&ui));
        });
    }

    #[test]
    fn moving_between_other_tabs_leaves_a_read_running() {
        on_gtk_thread(|| {
            let TestUi { ui, view_stack } = test_ui(discovered(PARAKEET_CONNECTED));
            ui.sync_active_backend();
            view_stack.set_visible_child_name("diagnostics");
            ui.trigger_snapshot("myna-parakeet");

            view_stack.set_visible_child_name("general");
            assert!(parakeet_loading(&ui));
        });
    }

    #[test]
    fn a_machine_with_no_active_backend_says_so_on_the_backend_tab() {
        on_gtk_thread(|| {
            let TestUi { ui, .. } = test_ui(discovered(PARAKEET_DISCONNECTED));
            ui.sync_active_backend();

            let page = ui
                .backend_nav
                .visible_page()
                .expect("the model tab shows a page")
                .downcast::<ui::StatusPage>()
                .expect("a status page");
            assert_eq!(page.status().title(), "No active model");
            assert_eq!(
                page.status().icon_name().as_deref(),
                Some("emblem-system-symbolic"),
                "the status page wears the Model tab's gear"
            );
        });
    }

    /// The General tab's model rows as (title, subtitle, the CSS node its
    /// check button draws: "radio" or "check").
    fn listed_models(ui: &BackendUi) -> Vec<(String, String, Option<String>)> {
        let group = ui
            .myna_selector
            .as_ref()
            .expect("general page")
            .model_group();
        descendants(group.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .map(|row| {
                let indicator = descendants(row.upcast_ref())
                    .into_iter()
                    .find(|widget| widget.is::<gtk::CheckButton>())
                    .and_then(|button| button.first_child())
                    .map(|node| node.css_name().to_string());
                (
                    row.title().to_string(),
                    row.subtitle().unwrap_or_default().to_string(),
                    indicator,
                )
            })
            .collect()
    }

    fn general_ui(connections: &str) -> Rc<BackendUi> {
        general_ui_with(connections, PARAKEET_SLOT)
    }

    fn general_ui_with(connections: &str, slots: &str) -> Rc<BackendUi> {
        ui::register_resources();
        let TestUi { ui, .. } = test_ui_with(
            discovered_with(connections, slots),
            Some(ui::MynaPage::new()),
        );
        ui.sync_active_backend();
        ui
    }

    #[test]
    fn a_single_connected_model_is_a_plain_row() {
        on_gtk_thread(|| {
            let ui = general_ui(PARAKEET_CONNECTED);
            assert_eq!(
                listed_models(&ui),
                [(
                    "Parakeet".to_owned(),
                    gettextrs::gettext("Fastest and most accurate in its 25 languages"),
                    None,
                )]
            );
        });
    }

    #[test]
    fn a_single_unconnected_model_keeps_its_radio_to_connect_it() {
        on_gtk_thread(|| {
            let ui = general_ui(PARAKEET_DISCONNECTED);
            assert_eq!(
                listed_models(&ui)
                    .into_iter()
                    .map(|(title, _, radio)| (title, radio))
                    .collect::<Vec<_>>(),
                [("Parakeet".to_owned(), Some("radio".to_owned()))]
            );
        });
    }

    #[test]
    fn two_models_are_exclusive_radios() {
        on_gtk_thread(|| {
            let ui = general_ui_with(
                PARAKEET_CONNECTED_WHISPER_INSTALLED,
                PARAKEET_AND_WHISPER_SLOTS,
            );
            assert_eq!(
                listed_models(&ui)
                    .into_iter()
                    .map(|(title, _, radio)| (title, radio))
                    .collect::<Vec<_>>(),
                [
                    ("Parakeet".to_owned(), Some("radio".to_owned())),
                    ("Whisper".to_owned(), Some("radio".to_owned())),
                ]
            );
            let marked = || {
                ui.model_rows
                    .borrow()
                    .iter()
                    .map(|row| row.radio.as_ref().expect("radio").is_active())
                    .collect::<Vec<_>>()
            };
            assert_eq!(marked(), [true, false]);
            ui.marking_models.set(true);
            ui.model_rows.borrow()[1]
                .radio
                .as_ref()
                .expect("radio")
                .set_active(true);
            ui.marking_models.set(false);
            assert_eq!(marked(), [false, true]);
        });
    }

    #[test]
    fn connecting_the_only_model_drops_its_radio() {
        on_gtk_thread(|| {
            let ui = general_ui(PARAKEET_DISCONNECTED);
            let request = ui.controller.begin_discovery();
            let snapshot = crate::domain::parse_connections(PARAKEET_CONNECTED, PARAKEET_SLOT)
                .expect("connections parse");
            ui.controller.complete_discovery(request, Ok(snapshot));
            ui.sync_active_backend();
            assert_eq!(
                listed_models(&ui)
                    .into_iter()
                    .map(|(title, _, radio)| (title, radio))
                    .collect::<Vec<_>>(),
                [("Parakeet".to_owned(), None)]
            );
        });
    }

    /// The pill's text when `row` shows one.
    fn shown_pill(row: &adw::ActionRow) -> Option<String> {
        descendants(row.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .find(|label| label.has_css_class("recommended-pill") && label.is_visible())
            .map(|label| label.label().to_string())
    }

    /// The button naming `row`'s languages, when its family is known.
    fn languages_button(row: &adw::ActionRow) -> Option<gtk::MenuButton> {
        descendants(row.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
    }

    /// The General tab's model rows in order, each with whether it shows the
    /// Recommended pill.
    fn recommended_rows(ui: &BackendUi) -> Vec<(String, bool)> {
        listed_rows(ui)
            .into_iter()
            .map(|row| (row.title().to_string(), shown_pill(&row).is_some()))
            .collect()
    }

    /// The General tab's model rows: title, pill text, languages summary.
    fn pills_and_languages(ui: &BackendUi) -> Vec<(String, Option<String>, Option<String>)> {
        listed_rows(ui)
            .into_iter()
            .map(|row| {
                (
                    row.title().to_string(),
                    shown_pill(&row),
                    languages_button(&row).map(|button| summary_of(&button)),
                )
            })
            .collect()
    }

    /// The languages `button` summarises in its tooltip. Its child must be
    /// an image so no arrow is drawn.
    fn summary_of(button: &gtk::MenuButton) -> String {
        assert!(
            button.child().and_downcast::<gtk::Image>().is_some(),
            "an image child, not the arrowed default"
        );
        button
            .tooltip_text()
            .expect("a summary tooltip")
            .to_string()
    }

    /// The names the popover of `button` lists, each with whether it stands out.
    fn open_languages(button: &gtk::MenuButton) -> (String, Vec<(String, bool)>) {
        button.popup();
        let popover = button.popover().expect("languages popover");
        let labels: Vec<gtk::Label> = descendants(popover.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .collect();
        let heading = labels
            .iter()
            .find(|label| {
                !label
                    .parent()
                    .is_some_and(|names| names.has_css_class("language-columns"))
            })
            .map(|label| label.label().to_string())
            .unwrap_or_default();
        let names = labels
            .iter()
            .filter(|label| {
                label
                    .parent()
                    .is_some_and(|names| names.has_css_class("language-columns"))
            })
            .map(|label| (label.label().to_string(), label.has_css_class("accent")))
            .collect();
        button.popdown();
        (heading, names)
    }

    fn listed_rows(ui: &BackendUi) -> Vec<adw::ActionRow> {
        let group = ui
            .myna_selector
            .as_ref()
            .expect("general page")
            .model_group();
        descendants(group.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .collect()
    }

    fn two_models_for(languages: &[&str]) -> Rc<BackendUi> {
        let ui = general_ui_with(
            PARAKEET_CONNECTED_WHISPER_INSTALLED,
            PARAKEET_AND_WHISPER_SLOTS,
        );
        ui.set_preferred_languages(languages.iter().map(|l| l.to_string()).collect());
        ui
    }

    #[test]
    fn the_recommended_model_sorts_first_with_a_pill() {
        on_gtk_thread(|| {
            assert_eq!(
                recommended_rows(&two_models_for(&["en_US", "en"])),
                [("Parakeet".to_owned(), true), ("Whisper".to_owned(), false)]
            );
            for locale in ["zh_CN", "ja_JP", "ko_KR", "yue"] {
                assert_eq!(
                    recommended_rows(&two_models_for(&[locale])),
                    [("Whisper".to_owned(), true), ("Parakeet".to_owned(), false)],
                    "{locale}: FunASR is not installed, so the best installed is"
                );
            }
            let ui = two_models_for(&["cy_GB"]);
            assert_eq!(
                recommended_rows(&ui),
                [("Whisper".to_owned(), true), ("Parakeet".to_owned(), false)]
            );
            let marked = ui
                .model_rows
                .borrow()
                .iter()
                .map(|row| row.radio.as_ref().expect("radio").is_active())
                .collect::<Vec<_>>();
            assert_eq!(marked, [false, true], "the connected Parakeet stays chosen");
        });
    }

    const THREE_MODEL_SLOTS: &str = "name: content\nslots:\n  \
         - myna-parakeet:provider:\n      content: inference-provider\n  \
         - myna-whisper:provider:\n      content: inference-provider\n  \
         - myna-fake-backend:provider:\n      content: inference-provider\n      task: speech-to-text\n";
    const PARAKEET_CONNECTED_TWO_MORE_INSTALLED: &str = "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend myna-parakeet:provider manual\n\
         content - myna-whisper:provider -\n\
         content - myna-fake-backend:provider -\n";

    #[test]
    fn the_pill_and_the_languages_name_the_user_language_in_itself() {
        on_gtk_thread(|| {
            let ui = general_ui_with(PARAKEET_CONNECTED_TWO_MORE_INSTALLED, THREE_MODEL_SLOTS);
            assert_eq!(
                pills_and_languages(&ui),
                [
                    ("Fake Backend".to_owned(), None, None),
                    (
                        "Parakeet".to_owned(),
                        None,
                        Some("English, Deutsch +23".to_owned())
                    ),
                    (
                        "Whisper".to_owned(),
                        None,
                        Some("English, 中文 +97".to_owned())
                    ),
                ],
                "before the language is known"
            );
            ui.set_preferred_languages(vec!["de_DE".to_owned(), "en".to_owned()]);
            assert_eq!(
                pills_and_languages(&ui),
                [
                    (
                        "Parakeet".to_owned(),
                        Some("Best for Deutsch".to_owned()),
                        Some("Deutsch, English +23".to_owned())
                    ),
                    ("Fake Backend".to_owned(), None, None),
                    (
                        "Whisper".to_owned(),
                        None,
                        Some("Deutsch, English +97".to_owned())
                    ),
                ]
            );
        });
    }

    #[test]
    fn a_model_lists_its_languages_on_demand_the_user_language_first() {
        on_gtk_thread(|| {
            let ui = two_models_for(&["de_DE"]);
            let window = adw::Window::builder()
                .default_width(800)
                .default_height(600)
                .content(ui.myna_selector.as_ref().expect("general page"))
                .build();
            window.present();
            let rows = listed_rows(&ui);
            let (heading, names) = open_languages(&languages_button(&rows[0]).expect("button"));
            assert_eq!(heading, "25 languages");
            assert_eq!(names.len(), 25);
            assert_eq!(names[0], ("Deutsch".to_owned(), true));
            assert_eq!(names[1], ("Čeština".to_owned(), false));
            assert_eq!(names.iter().filter(|(_, mine)| *mine).count(), 1);
            let button = languages_button(&rows[0]).expect("button");
            button.popup();
            let grid = descendants(button.popover().expect("popover").upcast_ref())
                .into_iter()
                .find(|widget| widget.has_css_class("language-columns"))
                .and_then(|widget| widget.downcast::<gtk::Grid>().ok())
                .expect("language grid");
            let mut cells: Vec<(i32, i32, gtk::Label)> = descendants(grid.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                .map(|label| {
                    let (column, row, _, _) = grid.query_child(&label);
                    (column, row, label)
                })
                .collect();
            cells.sort_by_key(|(column, row, _)| (*column, *row));
            let columns: Vec<Vec<String>> = (0..2)
                .map(|column| {
                    cells
                        .iter()
                        .filter(|(c, _, _)| *c == column)
                        .map(|(_, _, label)| label.label().to_string())
                        .collect()
                })
                .collect();
            assert!(grid.is_row_homogeneous(), "even rhythm down each column");
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0][..2], ["Deutsch", "Čeština"], "down the column");
            assert_eq!(columns[1][0], names[13].0, "then the next");
            button.popdown();

            ui.set_preferred_languages(vec!["ja_JP".to_owned()]);
            let parakeet = listed_rows(&ui)
                .into_iter()
                .find(|row| row.title() == "Parakeet")
                .expect("Parakeet listed");
            let (_, names) = open_languages(&languages_button(&parakeet).expect("button"));
            assert!(
                names.iter().all(|(_, mine)| !mine),
                "Parakeet lacks Japanese, so nothing stands out"
            );
            window.destroy();
        });
    }

    #[test]
    fn a_model_is_described_by_its_subtitle_then_its_pill() {
        assert_eq!(model_description(None, None), None);
        assert_eq!(model_description(Some(""), None), None);
        assert_eq!(
            model_description(Some("Fast"), None),
            Some("Fast".to_owned())
        );
        assert_eq!(
            model_description(None, Some("Recommended")),
            Some("Recommended".to_owned())
        );
        assert_eq!(
            model_description(Some("Fast"), Some("Recommended")),
            Some("Fast\nRecommended".to_owned())
        );
    }

    #[test]
    fn no_pill_until_the_language_is_known() {
        on_gtk_thread(|| {
            let ui = general_ui_with(PARAKEET_CONNECTED_TWO_MORE_INSTALLED, THREE_MODEL_SLOTS);
            let unknown = recommended_rows(&ui);
            assert_eq!(unknown.len(), 3);
            assert!(
                unknown.iter().all(|(_, pill)| !pill),
                "a pill before the language is known: {unknown:?}"
            );
            ui.set_preferred_languages(vec!["en_US".to_owned()]);
            assert_eq!(recommended_rows(&ui)[0], ("Parakeet".to_owned(), true));
            assert_eq!(
                recommended_rows(&ui)
                    .iter()
                    .filter(|(_, pill)| *pill)
                    .count(),
                1
            );
        });
    }

    #[test]
    fn the_language_is_read_off_the_main_thread_then_recommends() {
        on_gtk_thread(|| {
            let ui = general_ui_with(
                PARAKEET_CONNECTED_WHISPER_INSTALLED,
                PARAKEET_AND_WHISPER_SLOTS,
            );
            let main = std::thread::current().id();
            let (read_on_tx, read_on) = std::sync::mpsc::channel();
            ui.read_preferred_languages(move || {
                read_on_tx.send(std::thread::current().id()).ok();
                vec!["cy_GB".to_owned()]
            });
            let context = glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !recommended_rows(&ui)[0].1 && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(
                recommended_rows(&ui),
                [("Whisper".to_owned(), true), ("Parakeet".to_owned(), false)]
            );
            assert_ne!(read_on.recv().ok(), Some(main));
        });
    }

    const KNOWN_FAMILY_SLOTS: &str = "name: content\nslots:\n  \
         - myna-parakeet:provider:\n      content: inference-provider\n  \
         - myna-whisper:provider:\n      content: inference-provider\n  \
         - myna-funasr:provider:\n      content: inference-provider\n";
    const PARAKEET_CONNECTED_EVERY_FAMILY_INSTALLED: &str = "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend myna-parakeet:provider manual\n\
         content - myna-whisper:provider -\n\
         content - myna-funasr:provider -\n";

    const PARAKEET_AND_FUNASR_SLOTS: &str = "name: content\nslots:\n  \
         - myna-parakeet:provider:\n      content: inference-provider\n  \
         - myna-funasr:provider:\n      content: inference-provider\n";
    const PARAKEET_CONNECTED_FUNASR_INSTALLED: &str = "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend myna-parakeet:provider manual\n\
         content - myna-funasr:provider -\n";

    /// The hint Install more models shows, when it is shown.
    fn install_hint(ui: &BackendUi) -> Option<String> {
        let page = ui.myna_selector.as_ref().expect("general page");
        let hint = page.install_hint();
        (page.install_button().is_visible() && hint.is_visible()).then(|| hint.label().to_string())
    }

    #[test]
    fn install_more_models_hints_a_better_model_for_the_user_language() {
        on_gtk_thread(|| {
            let ui = two_models_for(&["zh_CN"]);
            assert_eq!(
                install_hint(&ui).as_deref(),
                Some("A better model for 中文 is available")
            );
            assert_eq!(install_hint(&two_models_for(&["en_US"])), None);
            let unknown = general_ui_with(
                PARAKEET_CONNECTED_WHISPER_INSTALLED,
                PARAKEET_AND_WHISPER_SLOTS,
            );
            assert_eq!(install_hint(&unknown), None, "no hint before the language");
            let dialog = ui.install_models();
            assert_eq!(
                offered_rows(&dialog)
                    .iter()
                    .map(|row| (row.title().to_string(), shown_pill(row)))
                    .collect::<Vec<_>>(),
                [("FunASR".to_owned(), Some("Best for 中文".to_owned()))],
                "the dialog keeps the pill on the best family"
            );
            let with_funasr = general_ui_with(
                PARAKEET_CONNECTED_FUNASR_INSTALLED,
                PARAKEET_AND_FUNASR_SLOTS,
            );
            with_funasr.set_preferred_languages(vec!["zh_CN".to_owned()]);
            assert_eq!(install_hint(&with_funasr), None);
            assert!(
                install_button_shown(&with_funasr),
                "Whisper is still offered"
            );
        });
    }

    fn install_button_shown(ui: &BackendUi) -> bool {
        ui.myna_selector
            .as_ref()
            .expect("general page")
            .install_button()
            .is_visible()
    }

    #[test]
    fn install_more_models_hides_once_every_known_family_is_installed() {
        on_gtk_thread(|| {
            assert!(install_button_shown(&general_ui(PARAKEET_CONNECTED)));
            assert!(install_button_shown(&general_ui_with(
                PARAKEET_CONNECTED_TWO_MORE_INSTALLED,
                THREE_MODEL_SLOTS,
            )));
            assert!(!install_button_shown(&general_ui_with(
                PARAKEET_CONNECTED_EVERY_FAMILY_INSTALLED,
                KNOWN_FAMILY_SLOTS,
            )));
        });
    }

    /// The install dialog's rows: title, subtitle, whether the pill shows,
    /// and the accessible description.
    fn offered(dialog: &ui::InstallModelsDialog) -> Vec<(String, String, bool)> {
        offered_rows(dialog)
            .iter()
            .map(|row| {
                (
                    row.title().to_string(),
                    row.subtitle().unwrap_or_default().to_string(),
                    shown_pill(row).is_some(),
                )
            })
            .collect()
    }

    fn offered_rows(dialog: &ui::InstallModelsDialog) -> Vec<adw::ActionRow> {
        descendants(dialog.families().upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .collect()
    }

    #[test]
    fn the_install_dialog_names_the_user_language_in_itself() {
        on_gtk_thread(|| {
            let ui = general_ui(PARAKEET_CONNECTED);
            ui.set_preferred_languages(vec!["ja_JP".to_owned()]);
            let shown: Vec<_> = offered_rows(&ui.install_models())
                .iter()
                .map(|row| {
                    (
                        row.title().to_string(),
                        shown_pill(row),
                        languages_button(row).map(|button| summary_of(&button)),
                    )
                })
                .collect();
            assert_eq!(
                shown,
                [
                    (
                        "FunASR".to_owned(),
                        Some("Best for 日本語".to_owned()),
                        Some("日本語, 中文 +3".to_owned())
                    ),
                    (
                        "Whisper".to_owned(),
                        None,
                        Some("日本語, English +97".to_owned())
                    ),
                ]
            );
        });
    }

    #[test]
    fn the_install_dialog_offers_the_missing_families_recommended_first() {
        on_gtk_thread(|| {
            let whisper = format!(
                "{}\u{a0}·\u{a0}198\u{a0}MB",
                gettextrs::gettext("Most languages, with uneven accuracy")
            );
            let funasr = format!(
                "{}\u{a0}·\u{a0}309\u{a0}MB",
                gettextrs::gettext("Best for Chinese, Japanese and Korean")
            );
            let ui = general_ui(PARAKEET_CONNECTED);
            assert_eq!(
                offered(&ui.install_models()),
                [
                    ("Whisper".to_owned(), whisper.clone(), false),
                    ("FunASR".to_owned(), funasr.clone(), false),
                ],
                "no pill before the language is known"
            );
            ui.set_preferred_languages(vec!["ja_JP".to_owned()]);
            assert_eq!(
                offered(&ui.install_models()),
                [
                    ("FunASR".to_owned(), funasr.clone(), true),
                    ("Whisper".to_owned(), whisper.clone(), false),
                ]
            );
            ui.set_preferred_languages(vec!["en_US".to_owned()]);
            assert_eq!(
                offered(&ui.install_models()),
                [
                    ("Whisper".to_owned(), whisper, false),
                    ("FunASR".to_owned(), funasr, false),
                ],
                "the installed Parakeet keeps the pill on the General tab"
            );
        });
    }

    #[test]
    fn install_more_models_opens_the_dialog() {
        on_gtk_thread(|| {
            let (ui, _, window) = refocusable_ui();
            let opened = open_install_dialogs;
            assert!(opened().is_empty());
            ui.myna_selector
                .as_ref()
                .expect("general page")
                .install_button()
                .emit_clicked();
            let dialogs = opened();
            assert_eq!(dialogs.len(), 1);
            assert_eq!(offered(&dialogs[0]).len(), 2);
            dialogs[0].force_close();
            window.destroy();
        });
    }

    #[test]
    fn install_hands_its_row_family_over() {
        on_gtk_thread(|| {
            use myna_core::language::ModelFamily as Family;
            let chosen = Rc::new(RefCell::new(Vec::new()));
            let dialog = ui::InstallModelsDialog::new();
            let offer = InstallOffer {
                families: vec![Family::Whisper, Family::FunAsr],
                recommended: None,
                user_language: None,
            };
            let rows = offer_install(&dialog, &offer, false, {
                let chosen = Rc::clone(&chosen);
                Rc::new(move |family| chosen.borrow_mut().push(family))
            });
            assert!(rows.iter().all(|offered| !offered.row.is_activatable()));
            rows[1].control.button().emit_clicked();
            assert_eq!(*chosen.borrow(), [Family::FunAsr]);
        });
    }

    #[test]
    fn an_offer_says_what_it_downloads() {
        let whisper = family_offer(myna_core::language::ModelFamily::Whisper, false);
        assert_eq!(
            offer_subtitle(Some("Most languages, with uneven accuracy"), whisper),
            "Most languages, with uneven accuracy\u{a0}·\u{a0}198\u{a0}MB"
        );
        let gpu = family_offer(myna_core::language::ModelFamily::Whisper, true);
        assert_eq!(offer_subtitle(None, gpu), "up to 1.4\u{a0}GB");
    }

    #[test]
    fn a_lone_model_shows_its_pill_only_when_it_is_the_recommendation() {
        on_gtk_thread(|| {
            let ui = general_ui(PARAKEET_CONNECTED);
            ui.set_preferred_languages(vec!["de_DE".to_owned()]);
            assert_eq!(recommended_rows(&ui), [("Parakeet".to_owned(), true)]);
            ui.set_preferred_languages(vec!["ja_JP".to_owned()]);
            assert_eq!(recommended_rows(&ui), [("Parakeet".to_owned(), false)]);
        });
    }

    #[test]
    fn a_surface_failure_becomes_one_sentence_with_no_command_line() {
        let error = crate::domain::BackendSurfaceError::new(
            crate::domain::BackendSurface::Connections,
            "snap connections failed",
            "permission denied",
        );

        let problem = problem_from_surface_error(&error);
        assert_eq!(problem, "Model connections: snap connections failed");
    }

    #[test]
    fn a_switch_report_says_what_ran_and_what_is_connected_now() {
        let parakeet = BackendIdentity::new("myna-parakeet", "provider");
        let snapshot = crate::domain::ConnectionSnapshot::new(
            vec![parakeet.clone()],
            ActiveBackendState::Connected(parakeet),
        );
        let ran = || {
            vec![crate::domain::CommandResult::new(
                "snap",
                vec!["disconnect".to_owned(), "myna:backend".to_owned()],
                Some(1),
                "",
                "cannot disconnect",
            )]
        };
        let unread = crate::domain::BackendSurfaceError::new(
            crate::domain::BackendSurface::Connections,
            "snap connections failed",
            "",
        );

        let (summary, details) = switch_report(&SwitchOutcome::Failed {
            completed: ran(),
            error: crate::ports::SystemConfiguratorError::authorization_denied(
                "snap",
                Vec::new(),
                None,
                "access denied",
            ),
            final_snapshot: None,
            discovery_error: Some(unread.clone()),
        });
        assert_eq!(summary, "The change could not be made.");
        for fact in [
            "Final connections could not be verified: snap connections failed",
            "snap disconnect myna:backend",
            "cannot disconnect",
            "access denied",
        ] {
            assert!(details.contains(fact), "{fact:?} missing from {details}");
        }

        let (summary, details) = switch_report(&SwitchOutcome::Disagreed {
            completed: ran(),
            final_snapshot: snapshot.clone(),
        });
        assert_eq!(
            summary,
            "The change ran, but the connections do not match it."
        );
        assert!(details.contains("Final connections: connected"));
        assert!(details.contains("myna-parakeet"));
        assert!(details.contains("snap disconnect myna:backend"));

        let (summary, details) = switch_report(&SwitchOutcome::StaleDiscovery {
            final_snapshot: snapshot,
        });
        assert_eq!(
            summary,
            "The connections changed before the change could start. Try again."
        );
        assert!(details.contains("Final connections: connected"));
        assert!(!details.contains("snapd operations"));

        let (summary, details) = switch_report(&SwitchOutcome::FinalDiscoveryFailed {
            completed: Vec::new(),
            error: unread,
        });
        assert_eq!(
            summary,
            "The change ran, but the connections could not be read back."
        );
        assert!(details.contains("No snapd operations completed."));
        assert!(details.ends_with("snap connections failed"));
    }

    #[test]
    fn readback_failure_details_are_complete_copyable_and_privacy_safe() {
        let errors = [
            crate::domain::BackendSurfaceError::new(
                crate::domain::BackendSurface::ModelctlConfig,
                "command exited with status 1: no such key",
                "private config output",
            ),
            crate::domain::BackendSurfaceError::new(
                crate::domain::BackendSurface::Models,
                "model list unavailable",
                "",
            ),
        ];

        let details = read_back_failure_details(&errors);
        assert!(details.contains("command exited with status 1: no such key"));
        assert!(details.contains("model list unavailable"));
        assert!(!details.contains("private config output"));
        assert!(!details.contains("snap run"));
    }

    #[test]
    fn a_reconciliation_names_each_failed_surface_in_words() {
        let mut snapshot = crate::domain::BackendSnapshot::empty(BackendIdentity::new(
            "myna-parakeet",
            "provider",
        ));
        snapshot.add_error(crate::domain::BackendSurfaceError::new(
            crate::domain::BackendSurface::Engines,
            "command exited with status 1: permission denied",
            "",
        ));

        let summary = backend_snapshot_reconciliation_summary(&snapshot);
        assert!(
            summary
                .contains("    Available engines: command exited with status 1: permission denied"),
            "{summary}"
        );
        assert!(!summary.contains("Engines:"), "{summary}");
    }

    fn pending(operation_token: u64, cancellation: &CancellationToken) -> PendingChange {
        PendingChange {
            key: "streaming".to_owned(),
            value: ConfigValue::Boolean(true),
            operation_token,
            cancellation: cancellation.clone(),
            progress_message: "Applying…".to_owned(),
            progress_detail: None,
            focus: None,
        }
    }

    #[test]
    fn disappearing_backend_signals_apply_but_retains_gate_until_completion() {
        let coordinator = OperationCoordinator::new();
        let operation = coordinator.begin(OperationKind::BackendApply).unwrap();
        let token = operation.cancellation();
        let mut state = BTreeMap::from([(
            "myna-parakeet".to_owned(),
            pending(operation.token(), &token),
        )]);

        cancel_apply_state(&mut state, &coordinator, "myna-parakeet");
        cancel_apply_state(&mut state, &coordinator, "myna-whisper");

        assert!(token.is_cancelled());
        assert!(state.contains_key("myna-parakeet"));
        assert!(coordinator.begin(OperationKind::BackendSwitch).is_err());
        assert!(coordinator.complete(operation.token()));
        assert!(coordinator.begin(OperationKind::BackendSwitch).is_ok());
    }

    #[test]
    fn final_teardown_abandons_the_running_apply() {
        let coordinator = OperationCoordinator::new();
        let operation = coordinator.begin(OperationKind::BackendApply).unwrap();
        let token = operation.cancellation();
        let mut state = BTreeMap::from([(
            "myna-parakeet".to_owned(),
            pending(operation.token(), &token),
        )]);

        abandon_all_apply_state(&mut state, &coordinator);

        assert!(token.is_cancelled());
        assert!(state.is_empty());
        assert_eq!(coordinator.active(), None);
    }

    #[test]
    fn snapds_step_replaces_the_apply_state_on_the_row() {
        let token = CancellationToken::new();
        let mut change = pending(1, &token);
        assert_eq!(change.progress(), "Applying…");

        change.progress_detail = Some("Downloading".to_owned());
        assert_eq!(change.progress(), "Downloading");
    }

    #[test]
    fn a_download_reads_as_sizes() {
        let text = apply_progress_text(&ApplyProgress::Download {
            name: "model-small".to_owned(),
            done: 13_718_564,
            total: 483_966_976,
        });

        // GLib joins number and unit with a no-break space.
        assert_eq!(
            text,
            "Downloading model-small: 13.7\u{a0}MB of 484.0\u{a0}MB"
        );
    }

    fn parakeet_snapshot() -> crate::domain::BackendSnapshot {
        crate::domain::BackendSnapshot::empty(BackendIdentity::new("myna-parakeet", "provider"))
    }

    #[test]
    fn only_a_change_that_did_not_take_is_reported() {
        let (snapshot, notice) = apply_report(Err(ApplyFailure::CancelledExecution));
        assert!(snapshot.is_none() && notice.is_none(), "a dismissed prompt");

        let (snapshot, notice) = apply_report(Err(ApplyFailure::RestartReadiness {
            snapshot: Box::new(parakeet_snapshot()),
            message: "The model did not restart.\nserver (failed)".to_owned(),
        }));
        let notice = notice.expect("reported");
        assert!(snapshot.is_some());
        assert!(notice.failed);
        assert_eq!(
            notice.summary,
            "The setting was saved, but the model did not start again."
        );
        assert!(notice.details.contains("server (failed)"));

        let (snapshot, notice) = apply_report(Err(ApplyFailure::ReadBackUnavailable {
            snapshot: Box::new(parakeet_snapshot()),
            errors: vec![crate::domain::BackendSurfaceError::new(
                crate::domain::BackendSurface::ModelctlConfig,
                "modelctl get failed",
                "",
            )],
        }));
        let notice = notice.expect("reported");
        assert!(snapshot.is_some());
        assert!(!notice.failed, "not confirmed either way");
        assert_eq!(notice.details, "Model settings: modelctl get failed");

        let (snapshot, notice) = apply_report(Err(ApplyFailure::VerificationCancelled {
            commands: Vec::new(),
            snapshot: Box::new(parakeet_snapshot()),
        }));
        assert!(snapshot.is_some());
        assert!(notice.is_some_and(|notice| !notice.failed));

        let (snapshot, notice) = apply_report(Err(ApplyFailure::PartialExecution {
            snapshot: Box::new(parakeet_snapshot()),
            commands: Vec::new(),
            failure: Box::new(crate::ports::SystemConfiguratorError::Cancelled),
        }));
        assert!(snapshot.is_some());
        assert_eq!(
            notice.expect("reported").summary,
            "The change was interrupted after it started."
        );
    }

    /// Answers every discovery with the machine as it stands, and counts the
    /// connection reads.
    struct ChangingRepository {
        machine: RefCell<(&'static str, &'static str)>,
        reads: std::cell::Cell<usize>,
    }

    #[async_trait::async_trait(?Send)]
    impl BackendRepository for ChangingRepository {
        async fn discover(
            &self,
            cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            self.refresh(cancellation).await
        }

        async fn read_snapshot(
            &self,
            backend: &BackendIdentity,
            _cancellation: CancellationToken,
        ) -> crate::domain::BackendSnapshot {
            crate::domain::BackendSnapshot::empty(backend.clone())
        }

        async fn refresh(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            self.reads.set(self.reads.get() + 1);
            let (connections, slots) = *self.machine.borrow();
            Ok(crate::domain::parse_connections(connections, slots).expect("connections parse"))
        }
    }

    /// General over a Parakeet-only machine the test can change, with the
    /// first discovery done as startup does it.
    fn refocusable_ui() -> (Rc<BackendUi>, Rc<ChangingRepository>, adw::Window) {
        ui::register_resources();
        let repository = Rc::new(ChangingRepository {
            machine: RefCell::new((PARAKEET_CONNECTED, PARAKEET_SLOT)),
            reads: std::cell::Cell::new(0),
        });
        let controller = BackendController::new(repository.clone());
        let snapd = Rc::new(InstallMachine::new(Rc::clone(&repository)));
        let TestUi { ui, .. } = test_ui_ports(controller, Some(ui::MynaPage::new()), None, snapd);
        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(600)
            .content(&ui.overlay)
            .build();
        window.present();
        let request = ui.controller.begin_discovery();
        ui.controller.complete_discovery(
            request,
            Ok(
                crate::domain::parse_connections(PARAKEET_CONNECTED, PARAKEET_SLOT)
                    .expect("connections parse"),
            ),
        );
        (ui, repository, window)
    }

    fn settle(done: impl Fn() -> bool) {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !done() && Instant::now() < deadline {
            if !glib::MainContext::default().iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    /// snapd as Install more models sees it: each install a change that
    /// downloads while `downloading` holds a percentage, then is done and
    /// leaves Whisper installed, unless the test gives `install_snap` an
    /// answer; `pending` are changes someone else started.
    struct InstallMachine {
        repository: Rc<ChangingRepository>,
        answers: RefCell<
            std::collections::VecDeque<
                Result<Option<String>, crate::ports::SystemConfiguratorError>,
            >,
        >,
        downloading: std::cell::Cell<Option<u64>>,
        pending: RefCell<Vec<String>>,
        installs: RefCell<Vec<String>>,
    }

    impl InstallMachine {
        fn new(repository: Rc<ChangingRepository>) -> Self {
            Self {
                repository,
                answers: RefCell::default(),
                downloading: std::cell::Cell::new(None),
                pending: RefCell::default(),
                installs: RefCell::default(),
            }
        }
    }

    /// A download far larger than any offer, so percentages are snapd's own.
    const DOWNLOAD_UNIT: u64 = 1 << 32;

    #[async_trait::async_trait(?Send)]
    impl SystemConfigurator for InstallMachine {
        async fn user_daemons_enabled(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<bool, String> {
            Ok(true)
        }

        async fn set_up(
            &self,
            _plan: &crate::ports::SetUpPlan,
            _cancellation: CancellationToken,
        ) -> Result<(), crate::ports::SystemConfiguratorError> {
            unreachable!("the settings window never sets up")
        }

        async fn install_snap(
            &self,
            snap: &str,
            _cancellation: CancellationToken,
        ) -> Result<Option<String>, crate::ports::SystemConfiguratorError> {
            self.installs.borrow_mut().push(snap.to_owned());
            self.answers
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(Some(format!("change-{snap}"))))
        }

        async fn snap_change(
            &self,
            change_id: &str,
            _cancellation: CancellationToken,
        ) -> Result<crate::snap_changes::ChangeInProgress, String> {
            let snap = change_id.trim_start_matches("change-");
            let change = match self.downloading.get() {
                Some(done) => serde_json::json!({"id": change_id, "kind": "install-snap",
                    "ready": false, "status": "Doing", "summary": format!("Install \"{snap}\" snap"),
                    "tasks": [{"kind": "download-snap", "status": "Doing",
                        "progress": {"label": snap, "done": done * DOWNLOAD_UNIT, "total": 100 * DOWNLOAD_UNIT}}]}),
                None => {
                    self.pending.borrow_mut().retain(|pending| pending != snap);
                    *self.repository.machine.borrow_mut() = (
                        PARAKEET_CONNECTED_WHISPER_INSTALLED,
                        PARAKEET_AND_WHISPER_SLOTS,
                    );
                    serde_json::json!({"id": change_id, "kind": "install-snap",
                        "ready": true, "status": "Done", "summary": format!("Install \"{snap}\" snap")})
                }
            };
            crate::snap_changes::parse_change(change)
        }

        async fn changes_in_progress(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<Vec<crate::snap_changes::ChangeInProgress>, String> {
            crate::snap_changes::parse_changes(serde_json::Value::Array(
                self.pending
                    .borrow()
                    .iter()
                    .map(|snap| {
                        serde_json::json!({"id": format!("change-{snap}"), "kind": "install-snap",
                            "ready": false, "status": "Doing",
                            "summary": format!("Install \"{snap}\" snap from \"latest/edge\" channel")})
                    })
                    .collect(),
            ))
        }

        async fn execute_backend_switch(
            &self,
            _plan: &crate::active_backend::SwitchPlan,
            _cancellation: CancellationToken,
        ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure>
        {
            unreachable!("installing never switches models")
        }

        async fn restart_myna(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), crate::ports::SystemConfiguratorError> {
            unreachable!("installing never restarts Myna")
        }

        async fn apply_backend_config(
            &self,
            _preview: &ApplyPreview,
            _cancellation: CancellationToken,
        ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure>
        {
            unreachable!("installing never configures a model")
        }
    }

    /// General over a Parakeet-only machine whose snapd installs, with the
    /// dialog open.
    fn installing_ui() -> (
        Rc<BackendUi>,
        Rc<InstallMachine>,
        ui::InstallModelsDialog,
        adw::Window,
    ) {
        ui::register_resources();
        let repository = Rc::new(ChangingRepository {
            machine: RefCell::new((PARAKEET_CONNECTED, PARAKEET_SLOT)),
            reads: std::cell::Cell::new(0),
        });
        let snapd = Rc::new(InstallMachine::new(Rc::clone(&repository)));
        let controller = BackendController::new(repository);
        let TestUi { ui, .. } =
            test_ui_ports(controller, Some(ui::MynaPage::new()), None, snapd.clone());
        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(600)
            .content(&ui.overlay)
            .build();
        window.present();
        ui.trigger_discovery();
        settle(|| model_titles(&ui) == ["Parakeet"]);
        ui.present_install_models();
        let dialog = open_install_dialogs().pop().expect("dialog open");
        (ui, snapd, dialog, window)
    }

    /// Each offered row's title and what its install control shows: the
    /// button's label (bracketed while insensitive), the progress beside
    /// the spinner, or "Installed".
    fn offer_states(dialog: &ui::InstallModelsDialog) -> Vec<(String, String)> {
        offered_rows(dialog)
            .iter()
            .map(|row| {
                let control = descendants(row.upcast_ref())
                    .into_iter()
                    .find_map(|widget| widget.downcast::<ui::InstallControl>().ok())
                    .expect("an install control");
                let button = control.button();
                let progress = control.progress();
                let state = if button.is_visible() {
                    let label = button.label().unwrap_or_default().to_string();
                    if button.is_sensitive() {
                        label
                    } else {
                        format!("[{label}]")
                    }
                } else if progress.container.is_visible() {
                    assert!(progress.spinner.is_spinning());
                    progress.label.label().to_string()
                } else if control.installed().is_visible() {
                    "Installed".to_owned()
                } else {
                    String::new()
                };
                (row.title().to_string(), state)
            })
            .collect()
    }

    fn click_install(dialog: &ui::InstallModelsDialog, title: &str) {
        let row = offered_rows(dialog)
            .into_iter()
            .find(|row| row.title() == title)
            .expect("an offered row");
        descendants(row.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<ui::InstallControl>().ok())
            .expect("an install control")
            .button()
            .emit_clicked();
    }

    fn states(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(title, state)| ((*title).to_owned(), (*state).to_owned()))
            .collect()
    }

    #[test]
    fn installing_a_model_shows_its_progress_then_lists_it() {
        on_gtk_thread(|| {
            let (ui, snapd, dialog, window) = installing_ui();
            assert_eq!(
                offer_states(&dialog),
                states(&[("Whisper", "Install"), ("FunASR", "Install")])
            );
            snapd.downloading.set(Some(42));
            click_install(&dialog, "Whisper");
            settle(|| offer_states(&dialog)[0].1 == "Installing 42%");
            assert_eq!(
                offer_states(&dialog),
                states(&[("Whisper", "Installing 42%"), ("FunASR", "[Install]")]),
                "one install at a time"
            );
            assert_eq!(install_hint(&ui).as_deref(), Some("Installing Whisper 42%"));
            assert_eq!(*snapd.installs.borrow(), ["myna-whisper"]);

            snapd.downloading.set(None);
            settle(|| offer_states(&dialog)[0].1 == "Installed");
            assert_eq!(
                offer_states(&dialog),
                states(&[("Whisper", "Installed"), ("FunASR", "Install")])
            );
            assert_eq!(model_titles(&ui), ["Parakeet", "Whisper"]);
            assert_eq!(install_hint(&ui), None);
            assert!(toasts(&ui).is_empty(), "a success says nothing");
            dialog.force_close();
            window.destroy();
        });
    }

    #[test]
    fn a_dismissed_prompt_puts_install_back_silently() {
        on_gtk_thread(|| {
            let (ui, snapd, dialog, window) = installing_ui();
            snapd
                .answers
                .borrow_mut()
                .push_back(Err(crate::ports::SystemConfiguratorError::Cancelled));
            click_install(&dialog, "FunASR");
            settle(|| ui.model_installs.borrow().is_empty());
            assert_eq!(
                offer_states(&dialog),
                states(&[("Whisper", "Install"), ("FunASR", "Install")])
            );
            assert!(toasts(&ui).is_empty());
            dialog.force_close();
            window.destroy();
        });
    }

    #[test]
    fn a_refused_install_says_so_with_details() {
        on_gtk_thread(|| {
            let (ui, snapd, dialog, window) = installing_ui();
            snapd.answers.borrow_mut().push_back(Err(
                crate::ports::SystemConfiguratorError::snapd_execution(
                    crate::snap_install::install_request("myna-funasr"),
                    Some(401),
                    "access denied",
                ),
            ));
            click_install(&dialog, "FunASR");
            settle(|| !toasts(&ui).is_empty());
            assert_eq!(toasts(&ui), ["Installing FunASR failed", "Details"]);
            assert_eq!(
                offer_states(&dialog),
                states(&[("Whisper", "Install"), ("FunASR", "Install")])
            );
            dialog.force_close();
            window.destroy();
        });
    }

    #[test]
    fn an_install_started_elsewhere_is_followed_not_offered() {
        on_gtk_thread(|| {
            let (ui, snapd, first, window) = installing_ui();
            first.force_close();
            snapd.pending.borrow_mut().push("myna-whisper".to_owned());
            snapd.downloading.set(Some(7));
            ui.present_install_models();
            let dialog = open_install_dialogs().pop().expect("dialog open");
            settle(|| offer_states(&dialog)[0].1 == "Installing 7%");
            assert!(snapd.installs.borrow().is_empty(), "nothing installs twice");
            snapd.downloading.set(None);
            settle(|| offer_states(&dialog)[0].1 == "Installed");
            dialog.force_close();
            window.destroy();
        });
    }

    #[test]
    fn an_install_outlives_its_dialog() {
        on_gtk_thread(|| {
            let (ui, snapd, dialog, window) = installing_ui();
            snapd.downloading.set(Some(3));
            click_install(&dialog, "Whisper");
            settle(|| install_hint(&ui).as_deref() == Some("Installing Whisper 3%"));
            dialog.force_close();
            ui.present_install_models();
            let reopened = open_install_dialogs().pop().expect("dialog open");
            assert_eq!(offer_states(&reopened)[0].1, "Installing 3%");
            snapd.downloading.set(None);
            settle(|| model_titles(&ui) == ["Parakeet", "Whisper"]);
            reopened.force_close();
            window.destroy();
        });
    }

    fn model_titles(ui: &BackendUi) -> Vec<String> {
        listed_models(ui)
            .into_iter()
            .map(|(title, ..)| title)
            .collect()
    }

    fn open_install_dialogs() -> Vec<ui::InstallModelsDialog> {
        gtk::Window::list_toplevels()
            .into_iter()
            .flat_map(|window| descendants(&window))
            .filter_map(|widget| widget.downcast::<ui::InstallModelsDialog>().ok())
            .collect()
    }

    fn after_the_interval() -> Instant {
        Instant::now() + crate::backend_controller::FOCUS_REDISCOVERY_INTERVAL
    }

    #[test]
    fn regaining_focus_lists_a_model_installed_meanwhile() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            assert_eq!(model_titles(&ui), ["Parakeet"]);
            ui.present_install_models();
            let dialog = open_install_dialogs().pop().expect("dialog open");
            let offered_titles = |dialog: &ui::InstallModelsDialog| {
                offered(dialog)
                    .into_iter()
                    .map(|(title, ..)| title)
                    .collect::<Vec<_>>()
            };
            assert_eq!(offered_titles(&dialog), ["Whisper", "FunASR"]);

            *repository.machine.borrow_mut() = (
                PARAKEET_CONNECTED_WHISPER_INSTALLED,
                PARAKEET_AND_WHISPER_SLOTS,
            );
            ui.rediscover_on_focus(after_the_interval());
            settle(|| model_titles(&ui).len() == 2);

            assert_eq!(model_titles(&ui), ["Parakeet", "Whisper"]);
            assert_eq!(repository.reads.get(), 1);
            assert_eq!(
                open_install_dialogs(),
                std::slice::from_ref(&dialog),
                "the open dialog stays open"
            );
            assert_eq!(
                offer_states(&dialog),
                [
                    ("Whisper".to_owned(), "Installed".to_owned()),
                    ("FunASR".to_owned(), "Install".to_owned()),
                ],
                "an installed family stays, saying so"
            );

            *repository.machine.borrow_mut() = (
                PARAKEET_CONNECTED_EVERY_FAMILY_INSTALLED,
                KNOWN_FAMILY_SLOTS,
            );
            ui.rediscover_on_focus(
                after_the_interval() + crate::backend_controller::FOCUS_REDISCOVERY_INTERVAL,
            );
            settle(|| model_titles(&ui).len() == 3);
            assert_eq!(
                offer_states(&dialog),
                [
                    ("Whisper".to_owned(), "Installed".to_owned()),
                    ("FunASR".to_owned(), "Installed".to_owned()),
                ]
            );
            assert_eq!(
                open_install_dialogs().len(),
                1,
                "closing is the user's call"
            );
            dialog.force_close();
            window.destroy();
        });
    }

    #[test]
    fn installing_the_best_model_meanwhile_moves_the_pill_and_drops_the_hint() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            *repository.machine.borrow_mut() = (
                PARAKEET_CONNECTED_WHISPER_INSTALLED,
                PARAKEET_AND_WHISPER_SLOTS,
            );
            ui.rediscover_on_focus(after_the_interval());
            settle(|| model_titles(&ui).len() == 2);
            ui.set_preferred_languages(vec!["zh_CN".to_owned()]);
            assert_eq!(
                pills_and_languages(&ui)
                    .into_iter()
                    .map(|(title, pill, _)| (title, pill))
                    .collect::<Vec<_>>(),
                [
                    ("Whisper".to_owned(), Some("Best for 中文".to_owned())),
                    ("Parakeet".to_owned(), None),
                ]
            );
            assert!(install_hint(&ui).is_some());

            *repository.machine.borrow_mut() = (
                PARAKEET_CONNECTED_EVERY_FAMILY_INSTALLED,
                KNOWN_FAMILY_SLOTS,
            );
            ui.rediscover_on_focus(
                after_the_interval() + crate::backend_controller::FOCUS_REDISCOVERY_INTERVAL,
            );
            settle(|| model_titles(&ui).len() == 3);
            assert_eq!(
                recommended_rows(&ui),
                [
                    ("FunASR".to_owned(), true),
                    ("Parakeet".to_owned(), false),
                    ("Whisper".to_owned(), false),
                ]
            );
            assert_eq!(install_hint(&ui), None);
            window.destroy();
        });
    }

    #[test]
    fn a_refocus_that_brings_the_best_model_drops_the_hint_but_keeps_the_button() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            ui.set_preferred_languages(vec!["ko_KR".to_owned()]);
            assert_eq!(recommended_rows(&ui), [("Parakeet".to_owned(), false)]);
            assert_eq!(
                install_hint(&ui).as_deref(),
                Some("A better model for 한국어 is available")
            );
            *repository.machine.borrow_mut() = (
                PARAKEET_CONNECTED_FUNASR_INSTALLED,
                PARAKEET_AND_FUNASR_SLOTS,
            );
            ui.rediscover_on_focus(after_the_interval());
            settle(|| model_titles(&ui).len() == 2);
            assert_eq!(
                recommended_rows(&ui),
                [("FunASR".to_owned(), true), ("Parakeet".to_owned(), false)]
            );
            assert_eq!(install_hint(&ui), None);
            assert!(install_button_shown(&ui));
            window.destroy();
        });
    }

    #[test]
    fn focus_flapping_reads_the_machine_once() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            let at = after_the_interval();
            for _ in 0..5 {
                ui.rediscover_on_focus(at);
            }
            settle(|| !ui.controller.discovery_loading());
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            for _ in 0..5 {
                ui.rediscover_on_focus(at);
            }
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            assert_eq!(repository.reads.get(), 1);
            window.destroy();
        });
    }

    #[test]
    fn regaining_focus_during_a_model_switch_reads_nothing() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            let operation = ui
                .operation_coordinator
                .begin(OperationKind::BackendSwitch)
                .expect("no operation yet");
            ui.rediscover_on_focus(after_the_interval());
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            assert_eq!(repository.reads.get(), 0);
            ui.operation_coordinator.complete(operation.token());
            window.destroy();
        });
    }

    #[test]
    fn an_unchanged_machine_leaves_the_model_tab_alone() {
        on_gtk_thread(|| {
            let (ui, repository, window) = refocusable_ui();
            ui.view_stack.set_visible_child_name("model");
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            let shown = ui.backend_nav.visible_page();
            ui.rediscover_on_focus(after_the_interval());
            settle(|| repository.reads.get() == 1 && !ui.controller.discovery_loading());
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            assert_eq!(repository.reads.get(), 1);
            assert_eq!(ui.backend_nav.visible_page(), shown, "the page was rebuilt");
            assert_eq!(ui.view_stack.visible_child_name().as_deref(), Some("model"));
            window.destroy();
        });
    }

    /// A Parakeet machine that applies what it is asked and reads it back
    /// the way modelctl does, or refuses as told.
    struct ApplyMachine {
        configuration: RefCell<BTreeMap<String, String>>,
        model: RefCell<String>,
        refusal: RefCell<Option<crate::ports::SystemConfiguratorError>>,
        held: std::cell::Cell<bool>,
        /// Unset, a `set` runs but the value does not stick.
        keeping: std::cell::Cell<bool>,
        plans: RefCell<Vec<Vec<Vec<String>>>>,
    }

    impl ApplyMachine {
        fn new() -> Self {
            Self {
                configuration: RefCell::new(
                    include_str!("../tests/fixtures/modelctl-get.txt")
                        .lines()
                        .filter_map(|line| line.split_once(": "))
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                        .collect(),
                ),
                model: RefCell::new("small".to_owned()),
                refusal: RefCell::new(None),
                held: std::cell::Cell::new(false),
                keeping: std::cell::Cell::new(true),
                plans: RefCell::new(Vec::new()),
            }
        }

        fn plans(&self) -> Vec<Vec<Vec<String>>> {
            self.plans.borrow().clone()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl BackendRepository for ApplyMachine {
        async fn discover(
            &self,
            cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            self.refresh(cancellation).await
        }

        async fn read_snapshot(
            &self,
            backend: &BackendIdentity,
            _cancellation: CancellationToken,
        ) -> crate::domain::BackendSnapshot {
            let mut snapshot = crate::domain::BackendSnapshot::empty(
                backend.clone().with_modelctl_app("myna-parakeet.modelctl"),
            );
            let configuration: String = self
                .configuration
                .borrow()
                .iter()
                .map(|(key, value)| format!("{key}: {value}\n"))
                .collect();
            snapshot.set_modelctl_config(
                crate::domain::parse_modelctl_config(&configuration).expect("modelctl parse"),
            );
            snapshot.set_status(
                crate::domain::parse_status(include_str!("../tests/fixtures/modelctl-status.json"))
                    .expect("status parse"),
            );
            snapshot.set_models(
                crate::domain::parse_model_options(&format!(
                    r#"{{"active-model":"{}","models":[{{"name":"small"}},{{"name":"base"}}]}}"#,
                    self.model.borrow()
                ))
                .expect("models parse"),
            );
            snapshot
        }

        async fn refresh(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<crate::domain::ConnectionSnapshot, crate::domain::BackendSurfaceError> {
            Ok(
                crate::domain::parse_connections(PARAKEET_CONNECTED, PARAKEET_SLOT)
                    .expect("connections parse"),
            )
        }
    }

    #[async_trait::async_trait(?Send)]
    impl SystemConfigurator for ApplyMachine {
        async fn user_daemons_enabled(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<bool, String> {
            Ok(true)
        }

        async fn set_up(
            &self,
            _plan: &crate::ports::SetUpPlan,
            _cancellation: CancellationToken,
        ) -> Result<(), crate::ports::SystemConfiguratorError> {
            unreachable!("the settings window never sets up")
        }

        async fn install_snap(
            &self,
            _snap: &str,
            _cancellation: CancellationToken,
        ) -> Result<Option<String>, crate::ports::SystemConfiguratorError> {
            unreachable!("the Model tab never installs a snap")
        }

        async fn snap_change(
            &self,
            _change_id: &str,
            _cancellation: CancellationToken,
        ) -> Result<crate::snap_changes::ChangeInProgress, String> {
            unreachable!("the Model tab never follows an install")
        }

        async fn execute_backend_switch(
            &self,
            _plan: &crate::active_backend::SwitchPlan,
            _cancellation: CancellationToken,
        ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure>
        {
            unreachable!("the Model tab never switches models")
        }

        async fn restart_myna(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), crate::ports::SystemConfiguratorError> {
            unreachable!("the Model tab never restarts Myna")
        }

        async fn apply_backend_config(
            &self,
            preview: &ApplyPreview,
            _cancellation: CancellationToken,
        ) -> Result<Vec<crate::domain::CommandResult>, crate::ports::SystemConfiguratorFailure>
        {
            self.plans.borrow_mut().push(
                preview
                    .operations()
                    .iter()
                    .map(|operation| operation.arguments().to_vec())
                    .collect(),
            );
            while self.held.get() {
                glib::timeout_future(std::time::Duration::from_millis(5)).await;
            }
            if let Some(error) = self.refusal.borrow_mut().take() {
                return Err(crate::ports::SystemConfiguratorFailure::new(
                    Vec::new(),
                    error,
                ));
            }
            let mut results = Vec::new();
            for operation in preview.operations() {
                let arguments = operation.arguments();
                match arguments.get(2).map(String::as_str) {
                    Some("set") if self.keeping.get() => {
                        for assignment in &arguments[3..] {
                            if let Some((key, value)) = assignment.split_once('=') {
                                self.configuration
                                    .borrow_mut()
                                    .insert(key.to_owned(), value.to_owned());
                            }
                        }
                    }
                    Some("use-model") => *self.model.borrow_mut() = arguments[3].clone(),
                    _ => {}
                }
                results.push(crate::domain::CommandResult::new(
                    operation.executable(),
                    arguments.to_vec(),
                    Some(0),
                    "",
                    "",
                ));
            }
            Ok(results)
        }
    }

    /// The Model tab over [`ApplyMachine`], on screen with its page read.
    fn applying_ui() -> (Rc<BackendUi>, Rc<ApplyMachine>, adw::Window) {
        applying_ui_of_height(600)
    }

    fn applying_ui_of_height(height: i32) -> (Rc<BackendUi>, Rc<ApplyMachine>, adw::Window) {
        ui::register_resources();
        let machine = Rc::new(ApplyMachine::new());
        let controller = BackendController::new(machine.clone());
        let TestUi { ui, view_stack } = test_ui_ports(controller, None, None, machine.clone());
        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });
        ui.overlay.set_child(Some(&view_stack));
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(height)
            .content(&ui.overlay)
            .build();
        window.present();
        view_stack.set_visible_child_name("model");
        let request = ui.controller.begin_discovery();
        ui.controller.complete_discovery(
            request,
            Ok(
                crate::domain::parse_connections(PARAKEET_CONNECTED, PARAKEET_SLOT)
                    .expect("connections parse"),
            ),
        );
        settle(|| setting::<adw::SwitchRow>(&ui, "streaming").is_some());
        (ui, machine, window)
    }

    fn setting<T: IsA<gtk::Widget>>(ui: &BackendUi, key: &str) -> Option<T> {
        let page = ui.backend_nav.visible_page()?;
        find_named_descendant(page.upcast_ref(), &setting_widget_name(key))?
            .downcast()
            .ok()
    }

    fn applied(ui: &BackendUi) -> bool {
        ui.apply_state.borrow().is_empty() && ui.operation_coordinator.active().is_none()
    }

    fn toasts(ui: &BackendUi) -> Vec<String> {
        descendants(ui.overlay.upcast_ref())
            .into_iter()
            .filter(|widget| widget.type_().name() == "AdwToastWidget")
            .flat_map(|toast| descendants(&toast))
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .filter(|label| label.is_visible())
            .map(|label| label.label().to_string())
            .collect()
    }

    fn argv(plan: &[&[&str]]) -> Vec<Vec<String>> {
        plan.iter()
            .map(|operation| operation.iter().map(|&part| part.to_owned()).collect())
            .collect()
    }

    const STREAMING_OFF: &[&[&str]] = &[
        &[
            "run",
            "myna-parakeet.modelctl",
            "set",
            "streaming=false",
            "--assume-yes",
            "--no-restart",
        ],
        &["restart", "myna-parakeet"],
    ];

    #[test]
    fn a_switch_applies_its_change_at_once_and_a_success_says_nothing() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            let switch = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(switch.is_active());

            switch.set_active(false);
            settle(|| !machine.plans().is_empty() && applied(&ui));

            assert_eq!(machine.plans(), [argv(STREAMING_OFF)]);
            let switch = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(!switch.is_active(), "the row shows the read-back value");
            assert!(toasts(&ui).is_empty(), "{:?}", toasts(&ui));
            window.destroy();
        });
    }

    #[test]
    fn while_a_change_applies_its_row_says_so_and_the_others_wait() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            machine.held.set(true);
            setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .set_active(false);
            settle(|| !machine.plans().is_empty());

            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(!row.is_active(), "the row shows the value it changes to");
            assert_eq!(
                row.subtitle().as_deref(),
                Some("Applying and restarting the model…")
            );
            assert!(descendants(row.upcast_ref())
                .iter()
                .any(|widget| widget.is::<gtk::Spinner>()));
            let other = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert!(!other.is_sensitive(), "one change at a time");
            assert_eq!(
                ui.operation_coordinator.active(),
                Some(OperationKind::BackendApply)
            );

            ui.show_apply_progress(
                "myna-parakeet",
                Some("Downloading model-small: 1 MB of 2 MB".to_owned()),
            );
            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert_eq!(
                row.subtitle().as_deref(),
                Some("Downloading model-small: 1 MB of 2 MB")
            );
            ui.rebuild_backend_page("myna-parakeet");
            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert_eq!(
                row.subtitle().as_deref(),
                Some("Downloading model-small: 1 MB of 2 MB"),
                "a rebuild keeps the last progress"
            );

            machine.held.set(false);
            settle(|| applied(&ui));
            let other = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert!(other.is_sensitive());
            window.destroy();
        });
    }

    #[test]
    fn a_refused_change_puts_the_row_back_and_says_so() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            *machine.refusal.borrow_mut() =
                Some(crate::ports::SystemConfiguratorError::authorization_denied(
                    "pkexec",
                    Vec::new(),
                    Some(127),
                    "Not authorized",
                ));
            setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .set_active(false);
            settle(|| !machine.plans().is_empty() && applied(&ui) && !toasts(&ui).is_empty());

            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(row.is_active(), "the row shows what the model still has");
            assert_eq!(
                toasts(&ui),
                ["Changing “Streaming output” failed", "Details"]
            );
            let details = descendants(ui.overlay.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .find(|button| button.label().as_deref() == Some("Details"))
                .expect("details button");
            details.emit_clicked();
            let report = || {
                window
                    .visible_dialog()
                    .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
            };
            settle(|| report().is_some());
            let report = report().expect("the report opened");
            assert_eq!(
                report.heading().as_deref(),
                Some("Changing “Streaming output” failed")
            );
            assert!(report.details_text().contains("Not authorized"));
            window.destroy();
        });
    }

    #[test]
    fn a_dismissed_prompt_puts_the_row_back_silently() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            *machine.refusal.borrow_mut() = Some(crate::ports::SystemConfiguratorError::Cancelled);
            setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .set_active(false);
            settle(|| !machine.plans().is_empty() && applied(&ui));

            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(row.is_active());
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            assert!(toasts(&ui).is_empty(), "{:?}", toasts(&ui));
            window.destroy();
        });
    }

    #[test]
    fn an_entry_applies_on_enter_never_per_keystroke() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert_eq!(entry.text().as_str(), "300");
            for typed in ["6", "60"] {
                entry.set_text(typed);
            }
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }
            assert!(machine.plans().is_empty(), "typing applied");

            entry.emit_by_name::<()>("apply", &[]);
            settle(|| !machine.plans().is_empty() && applied(&ui));

            assert_eq!(
                machine.plans(),
                [argv(&[
                    &[
                        "run",
                        "myna-parakeet.modelctl",
                        "set",
                        "sleep-idle-seconds=60",
                        "--assume-yes",
                        "--no-restart",
                    ],
                    &["restart", "myna-parakeet"],
                ])]
            );
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert_eq!(entry.text().as_str(), "60");
            window.destroy();
        });
    }

    #[test]
    fn an_invalid_entry_is_refused_before_any_prompt() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            entry.set_text("soon");
            entry.emit_by_name::<()>("apply", &[]);
            settle(|| !toasts(&ui).is_empty());

            assert!(machine.plans().is_empty());
            assert_eq!(
                toasts(&ui),
                ["Unload when idle (seconds): value must be a non-negative whole number"]
            );
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert_eq!(entry.text().as_str(), "300", "the row is put back");
            window.destroy();
        });
    }

    #[test]
    fn choosing_a_model_applies_it() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            let combo = setting::<adw::ComboRow>(&ui, "model").expect("model row");
            assert_eq!(combo.selected(), 0);

            combo.set_selected(1);
            settle(|| !machine.plans().is_empty() && applied(&ui));

            assert_eq!(
                machine.plans()[0][0],
                [
                    "run",
                    "myna-parakeet.modelctl",
                    "use-model",
                    "base",
                    "--assume-yes",
                    "--no-restart",
                ]
            );
            let combo = setting::<adw::ComboRow>(&ui, "model").expect("model row");
            assert_eq!(combo.selected(), 1);
            window.destroy();
        });
    }

    #[test]
    fn the_changed_row_keeps_the_focus() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            let switch = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            switch.grab_focus();
            switch.set_active(false);
            settle(|| !machine.plans().is_empty() && applied(&ui));
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }

            let switch = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            let focus = gtk::prelude::GtkWindowExt::focus(&window).expect("a focus widget");
            assert!(
                focus == switch.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&switch),
                "focus is on {focus:?}"
            );
            window.destroy();
        });
    }

    #[test]
    fn the_changing_row_stays_legible_but_takes_no_input() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            machine.held.set(true);
            setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .set_active(false);
            settle(|| !machine.plans().is_empty());

            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            // Insensitive would dim its progress line with the rest.
            assert!(row.is_sensitive(), "the progress line is dimmed");
            assert!(!row.can_target() && !row.can_focus(), "the row takes input");

            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert!(!entry.is_sensitive());
            machine.held.set(false);
            settle(|| applied(&ui));
            let row = setting::<adw::SwitchRow>(&ui, "streaming").expect("streaming row");
            assert!(row.can_target() && row.can_focus());
            window.destroy();
        });
    }

    #[test]
    fn a_changing_entry_shows_its_progress_at_its_end() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            machine.held.set(true);
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            entry.set_text("60");
            entry.emit_by_name::<()>("apply", &[]);
            settle(|| !machine.plans().is_empty());

            let row = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            assert_eq!(row.text().as_str(), "60");
            assert!(row.is_sensitive() && !row.can_focus() && !row.can_target());
            let progress = || {
                let row = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
                find_named_descendant(row.upcast_ref(), APPLY_PROGRESS)
                    .and_downcast::<gtk::Label>()
                    .map(|label| label.label().to_string())
            };
            assert_eq!(
                progress().as_deref(),
                Some("Applying and restarting the model…")
            );
            assert!(
                !descendants(row.upcast_ref())
                    .iter()
                    .any(|widget| widget.has_css_class("edit-icon") && widget.is_visible()),
                "the pencil still offers to edit"
            );
            ui.show_apply_progress("myna-parakeet", Some("Waiting for snapd".to_owned()));
            assert_eq!(progress().as_deref(), Some("Waiting for snapd"));
            machine.held.set(false);
            settle(|| applied(&ui));
            window.destroy();
        });
    }

    /// The scrolled window of the shown page, not of a row inside it.
    fn scrolled(ui: &BackendUi) -> gtk::ScrolledWindow {
        let page = ui
            .backend_nav
            .visible_page()
            .and_downcast::<ui::BackendPage>()
            .expect("backend page shown");
        descendants(page.preferences_page().upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
            .last()
            .expect("the page scrolls")
    }

    #[test]
    fn a_rebuild_keeps_the_page_where_the_user_scrolled_it() {
        on_gtk_thread(|| {
            let (ui, _machine, window) = applying_ui_of_height(240);
            let adjustment = scrolled(&ui).vadjustment();
            settle(|| adjustment.upper() > adjustment.page_size() + 100.0);
            adjustment.set_value(100.0);
            let shown = ui.backend_nav.visible_page();

            ui.rebuild_backend_page("myna-parakeet");
            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }

            assert_eq!(
                ui.backend_nav.visible_page(),
                shown,
                "the page was replaced"
            );
            assert_eq!(scrolled(&ui).vadjustment().value(), 100.0);
            window.destroy();
        });
    }

    #[test]
    fn applying_a_focused_entry_keeps_the_page_where_it_was() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui_of_height(240);
            let adjustment = scrolled(&ui).vadjustment();
            settle(|| adjustment.upper() > adjustment.page_size() + 100.0);
            let bottom = adjustment.upper() - adjustment.page_size();
            adjustment.set_value(bottom);
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            entry.grab_focus();
            entry.set_text("60");
            let settled = || {
                for _ in 0..50 {
                    glib::MainContext::default().iteration(false);
                }
                scrolled(&ui).vadjustment().value()
            };
            assert_eq!(settled(), bottom);

            machine.held.set(true);
            entry.emit_by_name::<()>("apply", &[]);
            settle(|| !machine.plans().is_empty());
            assert_eq!(settled(), bottom, "starting the change scrolled the page");
            let focus = gtk::prelude::GtkWindowExt::focus(&window);
            assert!(
                focus.is_none(),
                "the focus moved to {:?}",
                focus.map(|widget| widget.widget_name())
            );

            machine.held.set(false);
            settle(|| applied(&ui));
            assert_eq!(settled(), bottom, "finishing the change scrolled the page");
            let entry = setting::<adw::EntryRow>(&ui, "sleep-idle-seconds").expect("idle row");
            let focus = gtk::prelude::GtkWindowExt::focus(&window).expect("a focus widget");
            assert!(focus.is_ancestor(&entry), "the focus left the row");
            assert_eq!(entry.selection_bounds(), None, "the value is selected");
            window.destroy();
        });
    }

    #[test]
    fn a_value_the_model_does_not_keep_is_reported() {
        on_gtk_thread(|| {
            let (ui, machine, window) = applying_ui();
            machine.keeping.set(false);
            setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .set_active(false);
            settle(|| !machine.plans().is_empty() && applied(&ui) && !toasts(&ui).is_empty());

            assert!(setting::<adw::SwitchRow>(&ui, "streaming")
                .expect("streaming row")
                .is_active());
            assert_eq!(
                toasts(&ui),
                ["Changing “Streaming output” failed", "Details"]
            );
            descendants(ui.overlay.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .find(|button| button.label().as_deref() == Some("Details"))
                .expect("details button")
                .emit_clicked();
            let report = || {
                window
                    .visible_dialog()
                    .and_then(|dialog| dialog.downcast::<ui::OperationErrorDialog>().ok())
            };
            settle(|| report().is_some());
            let details = report().expect("the report opened").details_text();
            assert!(
                details.contains("streaming: asked for false, read back true"),
                "{details}"
            );
            window.destroy();
        });
    }
}
