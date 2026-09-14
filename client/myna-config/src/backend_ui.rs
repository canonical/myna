//! GTK/libadwaita wiring for backend pages and the dynamic sidebar.
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
    execute_switch, ActiveBackendController, BackendHealth, PrepareSwitchError, SwitchOutcome,
};
use crate::adapters::snap_backend::SnapBackendRepository;
use crate::adapters::system_configurator::PkexecSystemConfigurator;
use crate::backend_apply::{
    execute_backend_apply, prepare_backend_apply, ApplyFailure, ApplyPreview, ApplySuccess,
    PrepareApplyError, ValidationIssue,
};
use crate::backend_controller::{
    BackendController, BackendPage, BackendRow, ConnectionKind, ControllerEvent, DiscoveryRequest,
};
use crate::command::{CancellationToken, GioCommandRunner};
use crate::diagnostics::{
    self, present_diagnostics, BackendDiagnostic, DiagnosticConnection, DiagnosticInput,
    InstalledSnap, OnboardingState, RefreshPolicy, RefreshReason,
};
use crate::domain::{ActiveBackendState, BackendIdentity, ConfigValue, ServiceState};
use crate::markup::escape_markup;
use crate::operation_gate::{OperationCoordinator, OperationKind};
use crate::performance::PerformanceFacts;
use crate::ports::{BackendRepository, SystemConfigurator};
use crate::presentation::{ControlType, Sensitivity};
use crate::ui;

/// Pure refresh policy shared by every event-driven refresh. Never installs a
/// periodic timer; see [`crate::diagnostics::RefreshPolicy`].
fn refresh_policy() -> RefreshPolicy {
    RefreshPolicy::default()
}

/// Runtime coordinator that keeps sidebar + content in sync with a
/// [`BackendController`].
pub struct BackendUi {
    controller: Rc<BackendController>,
    configurator: Rc<dyn SystemConfigurator>,
    sidebar_list: gtk::ListBox,
    split_view: adw::NavigationSplitView,
    overlay: adw::ToastOverlay,
    myna_row: gtk::ListBoxRow,
    myna_page: RefCell<Option<adw::NavigationPage>>,
    myna_selector: Option<ui::MynaPage>,
    operation_coordinator: OperationCoordinator,
    active_backend: ActiveBackendController,
    diagnostics_row: gtk::ListBoxRow,
    diagnostics_page: RefCell<Option<adw::NavigationPage>>,
    backend_rows: RefCell<BTreeMap<String, ui::SidebarRow>>,
    backend_pages: RefCell<BTreeMap<String, adw::NavigationPage>>,
    apply_state: RefCell<BTreeMap<String, BackendApplyState>>,
    selected: RefCell<Selection>,
    installed_snaps: RefCell<Vec<InstalledSnap>>,
    inventory_complete: std::cell::Cell<bool>,
    inventory_failure: RefCell<Option<String>>,
    /// The last clock probe and pressure reading. Refreshed with every
    /// discovery, off the main thread, so the page never spins a core itself.
    performance: RefCell<Option<PerformanceFacts>>,
    last_diagnostics_refresh: std::cell::Cell<Option<Instant>>,
}

#[derive(Clone, Debug)]
enum Selection {
    Myna,
    Backend(String),
    Diagnostics,
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

#[derive(Clone, Debug, Default)]
struct BackendApplyView {
    in_progress: bool,
    cancellable: bool,
    progress_message: Option<String>,
    feedback: Option<ApplyFeedback>,
}

#[derive(Clone, Debug)]
struct ApplyFeedback {
    title: String,
    description: String,
}

#[derive(Clone, Debug, Default)]
struct BackendApplyState {
    confirmation_pending: bool,
    operation_token: Option<u64>,
    operation_cancellation: Option<CancellationToken>,
    progress_message: Option<String>,
    cancellation: Option<CancellationToken>,
    feedback: Option<ApplyFeedback>,
}

impl BackendApplyState {
    fn view(&self) -> BackendApplyView {
        BackendApplyView {
            in_progress: self.confirmation_pending || self.cancellation.is_some(),
            cancellable: self.cancellation.is_some(),
            progress_message: self.progress_message.clone(),
            feedback: self.feedback.clone(),
        }
    }

    fn cancel(&mut self) {
        if let Some(token) = self.operation_cancellation.take() {
            token.cancel();
        }
        if let Some(token) = self.cancellation.take() {
            token.cancel();
        }
        self.progress_message = None;
    }
}

fn remove_apply_state(
    state: &mut BTreeMap<String, BackendApplyState>,
    coordinator: &OperationCoordinator,
    snap_name: &str,
) {
    let started = state
        .get(snap_name)
        .is_some_and(|entry| entry.cancellation.is_some());
    if started {
        let entry = state
            .get_mut(snap_name)
            .expect("started apply state still exists");
        let operation_token = entry.operation_token;
        entry.cancel();
        if let Some(token) = operation_token {
            coordinator.cancel(token);
        }
    } else if let Some(mut entry) = state.remove(snap_name) {
        let operation_token = entry.operation_token;
        entry.cancel();
        if let Some(token) = operation_token {
            coordinator.abandon(token);
        }
    }
}

fn abandon_all_apply_state(
    state: &mut BTreeMap<String, BackendApplyState>,
    coordinator: &OperationCoordinator,
) {
    for entry in state.values_mut() {
        if let Some(token) = entry.operation_token {
            coordinator.abandon(token);
        }
        entry.cancel();
    }
}

impl BackendUi {
    pub fn install(
        split_view: &adw::NavigationSplitView,
        overlay: &adw::ToastOverlay,
        myna_row: gtk::ListBoxRow,
        myna_page: adw::NavigationPage,
        diagnostics_row: gtk::ListBoxRow,
        diagnostics_page: adw::NavigationPage,
        sidebar_list: gtk::ListBox,
    ) -> Rc<Self> {
        let repository: Rc<dyn BackendRepository> =
            Rc::new(SnapBackendRepository::new(Arc::new(GioCommandRunner)));
        let configurator: Rc<dyn SystemConfigurator> =
            Rc::new(PkexecSystemConfigurator::new(Arc::new(GioCommandRunner)));
        Self::install_with_ports(
            repository,
            configurator,
            split_view,
            overlay,
            myna_row,
            myna_page,
            diagnostics_row,
            diagnostics_page,
            sidebar_list,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn install_with_repository(
        repository: Rc<dyn BackendRepository>,
        split_view: &adw::NavigationSplitView,
        overlay: &adw::ToastOverlay,
        myna_row: gtk::ListBoxRow,
        myna_page: adw::NavigationPage,
        diagnostics_row: gtk::ListBoxRow,
        diagnostics_page: adw::NavigationPage,
        sidebar_list: gtk::ListBox,
    ) -> Rc<Self> {
        let configurator: Rc<dyn SystemConfigurator> =
            Rc::new(PkexecSystemConfigurator::new(Arc::new(GioCommandRunner)));
        Self::install_with_ports(
            repository,
            configurator,
            split_view,
            overlay,
            myna_row,
            myna_page,
            diagnostics_row,
            diagnostics_page,
            sidebar_list,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn install_with_ports(
        repository: Rc<dyn BackendRepository>,
        configurator: Rc<dyn SystemConfigurator>,
        split_view: &adw::NavigationSplitView,
        overlay: &adw::ToastOverlay,
        myna_row: gtk::ListBoxRow,
        myna_page: adw::NavigationPage,
        diagnostics_row: gtk::ListBoxRow,
        diagnostics_page: adw::NavigationPage,
        sidebar_list: gtk::ListBox,
    ) -> Rc<Self> {
        let controller = BackendController::new(repository);
        let myna_selector = myna_page.clone().downcast::<ui::MynaPage>().ok();
        let operation_coordinator = OperationCoordinator::new();
        let ui = Rc::new(Self {
            controller,
            configurator,
            sidebar_list: sidebar_list.clone(),
            split_view: split_view.clone(),
            overlay: overlay.clone(),
            myna_row: myna_row.clone(),
            myna_page: RefCell::new(Some(myna_page)),
            myna_selector,
            active_backend: ActiveBackendController::with_coordinator(
                crate::domain::ConnectionSnapshot::new(
                    Vec::new(),
                    ActiveBackendState::Disconnected,
                ),
                operation_coordinator.clone(),
            ),
            operation_coordinator,
            diagnostics_row: diagnostics_row.clone(),
            diagnostics_page: RefCell::new(Some(diagnostics_page)),
            backend_rows: RefCell::new(BTreeMap::new()),
            backend_pages: RefCell::new(BTreeMap::new()),
            apply_state: RefCell::new(BTreeMap::new()),
            selected: RefCell::new(Selection::Myna),
            installed_snaps: RefCell::new(Vec::new()),
            inventory_complete: std::cell::Cell::new(false),
            inventory_failure: RefCell::new(None),
            performance: RefCell::new(None),
            last_diagnostics_refresh: std::cell::Cell::new(None),
        });

        ui.connect_sidebar_selection();
        ui.connect_active_backend_selector();

        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });

        // Event-driven refresh only — no perpetual polling. See
        // `crate::diagnostics::RefreshPolicy`.
        debug_assert!(refresh_policy().periodic_interval().is_none());
        ui.trigger_discovery();

        ui
    }

    fn connect_active_backend_selector(self: &Rc<Self>) {
        let Some(page) = self.myna_selector.as_ref() else {
            return;
        };
        page.switch_backend_button().connect_clicked({
            let ui = Rc::downgrade(self);
            move |_| {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                if ui.active_backend.busy() {
                    return;
                }
                let Some(page) = ui.myna_selector.as_ref() else {
                    return;
                };
                let selected = page.active_backend_row().selected() as usize;
                let options = ui.active_backend.options();
                let Some(option) = options.get(selected) else {
                    return;
                };
                ui.begin_backend_switch(option.backend().clone());
            }
        });
    }

    fn begin_backend_switch(self: &Rc<Self>, selected: BackendIdentity) {
        let request = match self.active_backend.begin(selected) {
            Ok(request) if request.plan().is_noop() => {
                self.render_active_backend_selector();
                self.run_backend_switch(request, true);
                return;
            }
            Ok(request) => request,
            Err(PrepareSwitchError::BackendUnavailable(_)) => {
                self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                    "The selected backend is no longer installed. Refresh and choose again.",
                )));
                self.trigger_discovery();
                return;
            }
            Err(PrepareSwitchError::Busy) => return,
        };
        self.render_active_backend_selector();
        let dialog = ui::ActiveBackendDialog::new(request.plan().confirmation_text());
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(owner) = ui.upgrade() else {
                return;
            };
            let confirmed = dialog
                .choose_future(Some(owner.split_view.upcast_ref::<gtk::Widget>()))
                .await
                == "switch";
            owner.run_backend_switch(request, confirmed);
        });
    }

    fn run_backend_switch(
        self: &Rc<Self>,
        request: crate::active_backend::SwitchRequest,
        confirmed: bool,
    ) {
        let Some(repository) = self.controller.repository().cloned() else {
            self.active_backend.abandon();
            self.render_active_backend_selector();
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
                confirmed,
                configurator.as_ref(),
                repository.as_ref(),
                cancellation,
            )
            .await;
            coordinator.complete(operation_token);
            if let Some(ui) = ui.upgrade() {
                if ui.active_backend.complete(operation_token, outcome.clone()) {
                    ui.render_active_backend_selector();
                    ui.present_switch_outcome(&outcome);
                    ui.trigger_discovery();
                }
            }
        });
    }

    fn present_switch_outcome(&self, outcome: &SwitchOutcome) {
        let (message, error_dialog) = match outcome {
            SwitchOutcome::Applied { .. } => {
                (gettextrs::gettext("Active backend switched."), None)
            }
            SwitchOutcome::Noop { .. } => (
                gettextrs::gettext("That backend is already the sole active connection."),
                None,
            ),
            SwitchOutcome::Disagreed { .. } => (
                gettextrs::gettext(
                    "Commands completed, but the connections now differ from the requested state.",
                ),
                None,
            ),
            SwitchOutcome::StaleDiscovery { .. } => (
                gettextrs::gettext(
                    "Connections changed before authorization. Review the refreshed state and try again.",
                ),
                None,
            ),
            SwitchOutcome::Failed {
                error,
                discovery_error,
                completed,
                final_snapshot,
            } => {
                let concise = if discovery_error.is_some() {
                    gettextrs::gettext(
                        "Backend switch failed and final connections could not be refreshed.",
                    )
                } else {
                    gettextrs::gettext("Backend switch failed. Actual connections were refreshed.")
                };
                let details = switch_failure_details(
                    error,
                    discovery_error.as_ref(),
                    completed,
                    final_snapshot.as_ref(),
                );
                (
                    concise.clone(),
                    Some((
                        gettextrs::gettext("Backend switch failed"),
                        concise,
                        details,
                    )),
                )
            }
            SwitchOutcome::Cancelled {
                discovery_error, ..
            } => (
                if discovery_error.is_some() {
                    gettextrs::gettext(
                        "Backend switch cancelled, but final connections could not be refreshed.",
                    )
                } else {
                    gettextrs::gettext(
                        "Backend switch cancelled. Actual connections were refreshed; no rollback is claimed.",
                    )
                },
                None,
            ),
            SwitchOutcome::FinalDiscoveryFailed { error, completed } => {
                let concise = gettextrs::gettext("Could not verify final backend connections.");
                let details = final_discovery_details(error, completed);
                (
                    format!("{} {}", concise.clone(), diagnostics::redact_text(error.message())),
                    Some((
                        gettextrs::gettext("Backend switch verification failed"),
                        concise,
                        details,
                    )),
                )
            }
        };
        self.overlay.add_toast(adw::Toast::new(&message));
        if let Some((heading, summary, details)) = error_dialog {
            self.present_operation_error_dialog(&heading, &summary, &details);
        }
    }

    fn present_operation_error_dialog(&self, heading: &str, summary: &str, details: &str) {
        let dialog = ui::OperationErrorDialog::new(heading, summary, details);
        dialog.present(Some(self.split_view.upcast_ref::<gtk::Widget>()));
    }

    fn sync_active_backend(self: &Rc<Self>) {
        if !self
            .active_backend
            .set_snapshot(self.controller.connection_snapshot())
        {
            return;
        }
        for page in self.controller.pages() {
            self.active_backend
                .set_health(page.identity().snap_name(), backend_health(&page));
        }
        self.render_active_backend_selector();
    }

    fn render_active_backend_selector(&self) {
        let Some(page) = self.myna_selector.as_ref() else {
            return;
        };
        let options = self.active_backend.options();
        let labels = options
            .iter()
            .map(|option| display_title_for(option.backend().snap_name()))
            .collect::<Vec<_>>();
        let references = labels.iter().map(String::as_str).collect::<Vec<_>>();
        page.active_backend_row()
            .set_model(Some(&gtk::StringList::new(&references)));
        let subtitle = if !self.active_backend.verified() {
            gettextrs::gettext(
                "Final connections could not be verified. Switching is disabled until refresh succeeds.",
            )
        } else {
            match self.active_backend.snapshot().active_state() {
                ActiveBackendState::Disconnected => gettextrs::gettext(
                    "No backend is connected. Choose an installed backend to connect.",
                ),
                ActiveBackendState::MultiplyConnected(_) => gettextrs::gettext(
                    "Multiple backends are connected. Choose one to make it the sole active backend.",
                ),
                ActiveBackendState::Connected(_) => gettextrs::gettext(
                    "Choose which installed backend Myna uses for dictation.",
                ),
                ActiveBackendState::FailedSwitch { .. } => gettextrs::gettext(
                    "The previous switch did not complete. The displayed connections are the actual refreshed state.",
                ),
            }
        };
        page.active_backend_row()
            .set_subtitle(&escape_markup(&subtitle));
        page.active_backend_row().set_selected(
            self.active_backend
                .selected_index()
                .unwrap_or(gtk::INVALID_LIST_POSITION),
        );
        page.active_backend_row().set_sensitive(!options.is_empty());
        let button = page.switch_backend_button();
        let apply_active = self.operation_coordinator.active() == Some(OperationKind::BackendApply);
        button.set_sensitive(
            !options.is_empty()
                && self.active_backend.verified()
                && !apply_active
                && !self.active_backend.busy(),
        );
        let label = if self.active_backend.busy() {
            gettextrs::gettext("Switching…")
        } else {
            gettextrs::gettext("Switch")
        };
        button.set_label(&label);
    }

    fn connect_sidebar_selection(self: &Rc<Self>) {
        self.sidebar_list.connect_row_selected({
            let ui = Rc::downgrade(self);
            move |_, row| {
                let Some(ui) = ui.upgrade() else {
                    return;
                };
                let Some(row) = row else {
                    return;
                };
                ui.on_row_selected(row);
            }
        });
    }

    pub fn controller(&self) -> Rc<BackendController> {
        Rc::clone(&self.controller)
    }

    fn apply_state_view(&self, snap_name: &str) -> BackendApplyView {
        self.apply_state
            .borrow()
            .get(snap_name)
            .map(BackendApplyState::view)
            .unwrap_or_default()
    }

    fn set_apply_feedback(&self, snap_name: &str, title: String, description: String) {
        let mut state = self.apply_state.borrow_mut();
        let entry = state.entry(snap_name.to_owned()).or_default();
        entry.progress_message = None;
        entry.confirmation_pending = false;
        entry.operation_token = None;
        entry.operation_cancellation = None;
        entry.cancellation = None;
        entry.feedback = Some(ApplyFeedback { title, description });
    }

    fn clear_apply_feedback(&self, snap_name: &str) {
        if let Some(entry) = self.apply_state.borrow_mut().get_mut(snap_name) {
            entry.feedback = None;
        }
    }

    fn set_apply_progress(
        &self,
        snap_name: &str,
        message: String,
        cancellation: CancellationToken,
    ) {
        let mut state = self.apply_state.borrow_mut();
        let entry = state.entry(snap_name.to_owned()).or_default();
        entry.progress_message = Some(message);
        entry.confirmation_pending = false;
        entry.cancellation = Some(cancellation);
        entry.feedback = None;
    }

    fn finish_apply(self: &Rc<Self>, snap_name: &str, title: String, description: String) {
        if let Some(token) = self
            .apply_state
            .borrow()
            .get(snap_name)
            .and_then(|state| state.operation_token)
        {
            self.operation_coordinator.complete(token);
        }
        if self.controller.page(snap_name).is_none() {
            self.apply_state.borrow_mut().remove(snap_name);
            self.overlay
                .add_toast(adw::Toast::new(&format!("{title}: {description}")));
            self.render_active_backend_selector();
            return;
        }
        self.set_apply_feedback(snap_name, title.clone(), description.clone());
        self.overlay
            .add_toast(adw::Toast::new(&format!("{title}: {description}")));
        self.rebuild_backend_page(snap_name);
        self.render_active_backend_selector();
    }

    fn cancel_apply(self: &Rc<Self>, snap_name: &str) {
        if let Some(entry) = self.apply_state.borrow_mut().get_mut(snap_name) {
            if let Some(token) = entry.cancellation.clone() {
                token.cancel();
                entry.progress_message = Some(gettextrs::gettext("Cancelling backend apply…"));
            }
        }
        self.rebuild_backend_page(snap_name);
    }

    fn begin_apply(self: &Rc<Self>, snap_name: &str) {
        if self.apply_state_view(snap_name).in_progress {
            return;
        }
        let Some(page) = self.controller.page(snap_name) else {
            return;
        };
        match prepare_backend_apply(&page) {
            Ok(preview) => {
                let operation = match self
                    .operation_coordinator
                    .begin(OperationKind::BackendApply)
                {
                    Ok(operation) => operation,
                    Err(_) => {
                        self.overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                            "Finish the active backend operation before applying changes.",
                        )));
                        return;
                    }
                };
                {
                    let mut states = self.apply_state.borrow_mut();
                    let state = states.entry(snap_name.to_owned()).or_default();
                    state.confirmation_pending = true;
                    state.operation_token = Some(operation.token());
                    state.operation_cancellation = Some(operation.cancellation());
                    state.feedback = None;
                    state.progress_message =
                        Some(gettextrs::gettext("Waiting for change confirmation…"));
                }
                self.rebuild_backend_page(snap_name);
                self.render_active_backend_selector();
                let ui = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let Some(ui) = ui.upgrade() else {
                        return;
                    };
                    let confirmed = ui.confirm_apply(&preview).await;
                    if !confirmed {
                        ui.finish_apply(
                            preview.backend().snap_name(),
                            gettextrs::gettext("Apply cancelled"),
                            gettextrs::gettext("No administrator authorization was requested."),
                        );
                        return;
                    }
                    ui.run_apply(preview);
                });
            }
            Err(PrepareApplyError::NoChanges) => {
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Nothing to apply"),
                    gettextrs::gettext("There are no staged backend changes."),
                );
            }
            Err(PrepareApplyError::Invalid(issues)) => {
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Invalid values"),
                    validation_issue_summary(&issues),
                );
            }
        }
    }

    async fn confirm_apply(&self, preview: &ApplyPreview) -> bool {
        let dialog = ui::ApplyDialog::new(preview.confirmation_text());
        dialog
            .choose_future(Some(self.split_view.upcast_ref::<gtk::Widget>()))
            .await
            == "apply"
    }

    fn run_apply(self: &Rc<Self>, preview: ApplyPreview) {
        let snap_name = preview.backend().snap_name().to_owned();
        if self.controller.page(&snap_name).is_none()
            || !self
                .apply_state
                .borrow()
                .get(&snap_name)
                .is_some_and(|state| state.confirmation_pending)
        {
            if let Some(token) = self
                .apply_state
                .borrow()
                .get(&snap_name)
                .and_then(|state| state.operation_token)
            {
                self.operation_coordinator.abandon(token);
            }
            remove_apply_state(
                &mut self.apply_state.borrow_mut(),
                &self.operation_coordinator,
                &snap_name,
            );
            return;
        }
        let operation_token = self
            .apply_state
            .borrow()
            .get(&snap_name)
            .and_then(|state| state.operation_token)
            .expect("confirmed apply has an operation token");
        let Some(repository) = self.controller.repository().cloned() else {
            self.operation_coordinator.abandon(operation_token);
            remove_apply_state(
                &mut self.apply_state.borrow_mut(),
                &self.operation_coordinator,
                &snap_name,
            );
            self.render_active_backend_selector();
            return;
        };
        let configurator = Rc::clone(&self.configurator);
        let cancellation = self
            .apply_state
            .borrow()
            .get(&snap_name)
            .and_then(|state| state.operation_cancellation.clone())
            .unwrap_or_default();
        self.set_apply_progress(
            &snap_name,
            apply_progress_message(preview.restart_impact()),
            cancellation.clone(),
        );
        self.rebuild_backend_page(&snap_name);
        let coordinator = self.operation_coordinator.clone();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = execute_backend_apply(
                &preview,
                true,
                configurator.as_ref(),
                repository.as_ref(),
                cancellation,
            )
            .await;
            coordinator.complete(operation_token);
            if let Some(ui) = ui.upgrade() {
                if ui.controller.page(&snap_name).is_some() {
                    ui.complete_apply(result, &snap_name);
                } else {
                    ui.apply_state.borrow_mut().remove(&snap_name);
                    ui.render_active_backend_selector();
                }
            }
        });
    }

    fn complete_apply(
        self: &Rc<Self>,
        result: Result<ApplySuccess, ApplyFailure>,
        snap_name: &str,
    ) {
        match result {
            Ok(success) => {
                self.controller
                    .apply_readback(snap_name, success.snapshot().clone());
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Changes applied"),
                    gettextrs::gettext("Requested values were confirmed by read-back."),
                );
            }
            Err(ApplyFailure::ReadBackMismatch {
                snapshot,
                mismatches,
            }) => {
                self.controller.apply_readback(snap_name, *snapshot);
                let heading = gettextrs::gettext("Read-back mismatch");
                let summary = gettextrs::gettext(
                    "The backend returned different values than those requested.",
                );
                let details = diagnostics::redact_text(&mismatch_summary(&mismatches));
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &details);
            }
            Err(ApplyFailure::RestartReadiness { snapshot, message }) => {
                self.controller.apply_readback(snap_name, *snapshot);
                let heading = gettextrs::gettext("Backend restart failed");
                let summary = gettextrs::gettext(
                    "The settings were written, but the backend did not become ready.",
                );
                let details = diagnostics::redact_text(&message);
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &details);
            }
            Err(ApplyFailure::ReadBackUnavailable { snapshot, errors }) => {
                self.controller.apply_readback(snap_name, *snapshot);
                let heading = gettextrs::gettext("Read-back failed");
                let summary = gettextrs::gettext(
                    "The settings were changed, but the saved values could not be verified.",
                );
                let details = read_back_failure_details(&errors);
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &details);
            }
            Err(ApplyFailure::PartialExecution {
                snapshot,
                commands,
                failure,
            }) => {
                let snapshot_ref = snapshot.as_ref().clone();
                let details = partial_execution_details(&failure, &commands, snapshot.as_ref());
                self.controller.apply_readback(snap_name, snapshot_ref);
                let (title, message) = partial_failure_presentation(&failure);
                let redacted_message = diagnostics::redact_text(&message);
                self.finish_apply(snap_name, title.clone(), redacted_message.clone());
                self.present_operation_error_dialog(&title, &redacted_message, &details);
            }
            Err(ApplyFailure::CancelledConfirmation) => {
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Apply cancelled"),
                    gettextrs::gettext("No administrator authorization was requested."),
                );
            }
            Err(ApplyFailure::CancelledExecution) => {
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Apply interrupted"),
                    gettextrs::gettext(
                        "The write outcome is uncertain. Refresh the backend before retrying.",
                    ),
                );
            }
            Err(ApplyFailure::VerificationCancelled { snapshot, .. }) => {
                self.controller.apply_readback(snap_name, *snapshot);
                self.finish_apply(
                    snap_name,
                    gettextrs::gettext("Verification cancelled"),
                    gettextrs::gettext(
                        "The write completed, but read-back was cancelled. Refresh to verify persisted values.",
                    ),
                );
            }
            Err(ApplyFailure::AuthorizationDenied { details }) => {
                let heading = gettextrs::gettext("Authorization denied");
                let summary = diagnostics::redact_text(details.message());
                let full = format!(
                    "{}\n\n{}\n{}",
                    heading,
                    gettextrs::gettext("snapd or polkit rejected the request."),
                    privileged_failure_details(&details),
                );
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &full);
            }
            Err(ApplyFailure::ValuesRejected { details }) => {
                let heading = gettextrs::gettext("Backend rejected changes");
                let summary = diagnostics::redact_text(details.message());
                let full = format!("{}\n\n{}", heading, privileged_failure_details(&details),);
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &full);
            }
            Err(ApplyFailure::Execution { details }) => {
                let heading = gettextrs::gettext("Apply failed");
                let summary = diagnostics::redact_text(details.message());
                let full = format!("{}\n\n{}", heading, privileged_failure_details(&details),);
                self.finish_apply(snap_name, heading.clone(), summary.clone());
                self.present_operation_error_dialog(&heading, &summary, &full);
            }
        }
    }

    fn on_row_selected(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        let selection = if row == &self.myna_row {
            Selection::Myna
        } else if row == &self.diagnostics_row {
            Selection::Diagnostics
        } else {
            let Some(name) = self
                .backend_rows
                .borrow()
                .iter()
                .find(|(_, backend_row)| backend_row.upcast_ref::<gtk::ListBoxRow>() == row)
                .map(|(name, _)| name.clone())
            else {
                return;
            };
            Selection::Backend(name)
        };
        let previous = self.selected.borrow().clone();
        if let Some(name) = backend_to_cancel(&previous, &selection) {
            self.controller.cancel_page(name);
        }
        *self.selected.borrow_mut() = selection.clone();
        self.show_selection(&selection);
        match &selection {
            Selection::Backend(name) => self.trigger_snapshot(name),
            Selection::Diagnostics => self.on_diagnostics_page_shown(),
            Selection::Myna => {}
        }
    }

    fn on_controller_event(self: &Rc<Self>, event: &ControllerEvent) {
        match event {
            ControllerEvent::DiscoveryStarted => {
                self.inventory_complete.set(false);
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::DiscoveryChanged => {
                self.sync_active_backend();
                self.sync_sidebar();
                for page in self.controller.pages() {
                    self.rebuild_backend_page(page.identity().snap_name());
                }
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::BackendChanged(identity) => {
                if let Some(page) = self.controller.page(identity.snap_name()) {
                    self.active_backend
                        .set_health(identity.snap_name(), backend_health(&page));
                    self.render_active_backend_selector();
                }
                self.rebuild_backend_page(identity.snap_name());
                self.rebuild_diagnostics_page();
            }
            ControllerEvent::BackendDirtyChanged(identity) => {
                if let Some(page) = self.controller.page(identity.snap_name()) {
                    if !page.dirty_keys().is_empty() {
                        self.clear_apply_feedback(identity.snap_name());
                    }
                    if let Some(row) = self.backend_rows.borrow().get(identity.snap_name()) {
                        update_backend_row(row, &page);
                    }
                }
                self.refresh_staged_changes(identity.snap_name());
            }
            ControllerEvent::DiscoveryFailed(error) => {
                self.rebuild_diagnostics_page();
                self.overlay.add_toast(adw::Toast::new(&format!(
                    "{}: {}",
                    gettextrs::gettext("Could not read installed backends"),
                    diagnostics::redact_text(error.message())
                )));
            }
        }
    }

    fn sync_sidebar(self: &Rc<Self>) {
        let pages = self.controller.pages();
        let mut previous = self.backend_rows.borrow_mut();
        let existing_names: Vec<String> = previous.keys().cloned().collect();
        let new_names: Vec<String> = pages
            .iter()
            .map(|page| page.identity().snap_name().to_owned())
            .collect();

        for name in &existing_names {
            if !new_names.contains(name) {
                if let Some(row) = previous.remove(name) {
                    self.sidebar_list.remove(&row);
                }
                self.backend_pages.borrow_mut().remove(name);
                remove_apply_state(
                    &mut self.apply_state.borrow_mut(),
                    &self.operation_coordinator,
                    name,
                );
                self.controller.cancel_page(name);
            }
        }

        let diagnostics_index = self.diagnostics_row.index();
        for (offset, page) in pages.iter().enumerate() {
            let name = page.identity().snap_name().to_owned();
            let row = previous.entry(name.clone()).or_insert_with(|| {
                let sidebar_row = ui::SidebarRow::new();
                sidebar_row.set_icon_name("audio-x-generic-symbolic");
                sidebar_row
                    .upcast_ref::<gtk::ListBoxRow>()
                    .set_selectable(true);
                let insertion_index = diagnostics_index.max(0) + offset as i32;
                self.sidebar_list
                    .insert(sidebar_row.upcast_ref::<gtk::Widget>(), insertion_index);
                sidebar_row
            });
            update_backend_row(row, page);
        }

        let selected = self.selected.borrow().clone();
        if let Selection::Backend(name) = &selected {
            if !previous.contains_key(name) {
                *self.selected.borrow_mut() = Selection::Myna;
                self.sidebar_list.select_row(Some(&self.myna_row));
                self.show_selection(&Selection::Myna);
            }
        }
        drop(previous);
    }

    /// Rebuilds only the staged-changes group, leaving the setting widgets
    /// alone. Staging happens while the user is typing, and destroying the
    /// entry mid-edit loses the keystrokes GTK has not delivered yet.
    fn refresh_staged_changes(self: &Rc<Self>, snap_name: &str) {
        let Some(page) = self.controller.page(snap_name) else {
            return;
        };
        let Some(widget) = self.backend_pages.borrow().get(snap_name).cloned() else {
            return;
        };
        let Ok(backend_page) = widget.downcast::<ui::BackendPage>() else {
            return;
        };
        let preferences = backend_page.preferences_page();
        if let Some(group) = find_named_descendant(preferences.upcast_ref(), STAGED_CHANGES_GROUP)
            .and_then(|group| group.downcast::<adw::PreferencesGroup>().ok())
        {
            preferences.remove(&group);
        }
        add_apply_group(&preferences, &page, self);
    }

    fn rebuild_backend_page(self: &Rc<Self>, snap_name: &str) {
        let focus = self.backend_focus(snap_name);
        let Some(page) = self.controller.page(snap_name) else {
            return;
        };
        if let Some(row) = self.backend_rows.borrow().get(snap_name) {
            update_backend_row(row, &page);
        }
        let widget = build_backend_page(&page, self);
        self.backend_pages
            .borrow_mut()
            .insert(snap_name.to_owned(), widget);
        if matches!(&*self.selected.borrow(), Selection::Backend(current) if current == snap_name) {
            self.show_selection(&Selection::Backend(snap_name.to_owned()));
            if let Some(focus) = focus {
                self.restore_backend_focus(&focus);
            }
        }
    }

    fn backend_focus(&self, snap_name: &str) -> Option<BackendFocus> {
        let page = self.backend_pages.borrow().get(snap_name)?.clone();
        let window = self.split_view.root()?.downcast::<gtk::Window>().ok()?;
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
        let Some(page) = self.split_view.content() else {
            return;
        };
        let Some(widget) = find_named_descendant(page.upcast_ref(), &focus.widget_name) else {
            return;
        };
        widget.grab_focus();
        if let (Some(focus), Ok(entry)) = (focus.entry.as_ref(), widget.downcast::<adw::EntryRow>())
        {
            if typed_text_stages_as(&focus.text, entry.text().as_str()) {
                entry.set_text(&focus.text);
            }
            entry.set_position(focus.cursor_position);
        }
    }

    fn rebuild_diagnostics_page(self: &Rc<Self>) {
        let focus = self.diagnostics_focus();
        let page = self.build_diagnostics_page();
        *self.diagnostics_page.borrow_mut() = Some(page);
        if matches!(&*self.selected.borrow(), Selection::Diagnostics) {
            self.show_selection(&Selection::Diagnostics);
            self.restore_diagnostics_focus(focus);
        }
    }

    fn diagnostics_focus(&self) -> Option<DiagnosticsFocus> {
        let page = self
            .diagnostics_page
            .borrow()
            .as_ref()?
            .clone()
            .downcast::<ui::DiagnosticsPage>()
            .ok()?;
        let window = self.split_view.root()?.downcast::<gtk::Window>().ok()?;
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
        let preferences = widget.preferences_page();

        let pages = self.controller.pages();
        let discovery_error = self.controller.last_discovery_error();
        let discovery_loading = self.controller.discovery_loading();

        let snaps = self.installed_snaps.borrow().clone();
        let input = DiagnosticInput {
            inventory_complete: self.inventory_complete.get(),
            machine: Some(crate::machine::machine_facts()),
            daemon: crate::machine::snap_process("myna"),
            drops: crate::machine::audio_drops(),
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
        report_view.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Diagnostic report",
        ))]);

        // Onboarding surface, using structured, translated strings.
        if let Some(command) = report.onboarding_command() {
            let (title, description) = match report.onboarding() {
                OnboardingState::NoMyna => (
                    gettextrs::gettext("Install Myna to get started"),
                    gettextrs::gettext(
                        "The Myna snap is not installed. Copy this command and run it in a terminal — installation is never automatic.",
                    ),
                ),
                OnboardingState::NoBackend => (
                    gettextrs::gettext("Install a backend to enable dictation"),
                    gettextrs::gettext(
                        "No inference backend was discovered. Copy this command and run it in a terminal — installation is never automatic.",
                    ),
                ),
                OnboardingState::Ready => (String::new(), String::new()),
                OnboardingState::Unavailable => (String::new(), String::new()),
            };
            let group = adw::PreferencesGroup::builder()
                .title(title)
                .description(description)
                .build();
            let row = adw::ActionRow::builder()
                .title(command)
                .subtitle(gettextrs::gettext("Copy and run in a terminal"))
                .build();
            row.add_css_class("monospace");
            let copy = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(gettextrs::gettext("Copy install command"))
                .build();
            copy.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
                "Copy install command",
            ))]);
            let command_owned = command.to_owned();
            let overlay = self.overlay.clone();
            copy.connect_clicked(move |button| {
                let display = button.display();
                display.clipboard().set_text(&command_owned);
                overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                    "Install command copied to clipboard",
                )));
            });
            row.add_suffix(&copy);
            row.set_activatable_widget(Some(&copy));
            group.add(&row);
            preferences.add(&group);
        }

        widget.upcast()
    }

    fn on_diagnostics_page_shown(self: &Rc<Self>) {
        // Selecting Diagnostics performs an on-demand refresh consistent with
        // `RefreshReason::DiagnosticsRequested`.
        self.on_diagnostics_requested();
    }

    fn show_selection(self: &Rc<Self>, selection: &Selection) {
        let page = match selection {
            Selection::Myna => self.myna_page.borrow().clone(),
            Selection::Diagnostics => self.diagnostics_page.borrow().clone(),
            Selection::Backend(name) => {
                self.backend_pages.borrow().get(name).cloned().or_else(|| {
                    self.controller
                        .page(name)
                        .map(|snapshot_page| build_backend_page(&snapshot_page, self))
                })
            }
        };
        if let Some(page) = page {
            self.split_view.set_content(Some(&page));
            self.split_view.set_show_content(true);
            if self.split_view.is_collapsed() {
                let _ = page.child_focus(gtk::DirectionType::TabForward);
            }
        }
    }

    fn trigger_discovery(self: &Rc<Self>) {
        let Some(repository) = self.controller.repository().cloned() else {
            return;
        };
        let request: DiscoveryRequest = self.controller.begin_discovery();
        let token = request.token();
        let inventory_token = token.clone();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // The probe loads a core for a fraction of a second; it runs on
            // the blocking pool alongside the snapd reads, not on this thread.
            let probe = gio::spawn_blocking(crate::performance::performance_facts);
            let inventory = repository.installed_snaps(inventory_token).await;
            let result = repository.refresh(token).await;
            let performance = probe.await.ok();
            if let Some(ui) = ui.upgrade() {
                let accepted = ui.controller.complete_discovery(request, result);
                if !accepted {
                    return;
                }
                if performance.is_some() {
                    *ui.performance.borrow_mut() = performance;
                }
                match inventory {
                    Ok(snaps) => {
                        *ui.installed_snaps.borrow_mut() = snaps;
                        *ui.inventory_failure.borrow_mut() = None;
                    }
                    Err(error) => {
                        ui.installed_snaps.borrow_mut().clear();
                        *ui.inventory_failure.borrow_mut() =
                            Some(problem_from_surface_error(&error));
                    }
                }
                ui.inventory_complete.set(true);
                ui.rebuild_diagnostics_page();
                if matches!(&*ui.selected.borrow(), Selection::Diagnostics) {
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
            .is_some_and(|last| now.saturating_duration_since(last) < refresh_policy().debounce())
        {
            return;
        }
        self.last_diagnostics_refresh.set(Some(now));
        // Refresh policy budget for a user-initiated diagnostics request:
        // one inventory refresh plus one snapshot per visible backend.
        let plan = refresh_policy().plan(
            RefreshReason::DiagnosticsRequested,
            self.controller.pages().len(),
        );
        debug_assert!(
            plan.processes()
                <= diagnostics::APP_REFRESH_PROCESS_BUDGET
                    + self.controller.pages().len() * diagnostics::BACKEND_REFRESH_PROCESS_BUDGET
        );
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
    }
}

fn backend_health(page: &BackendPage) -> BackendHealth {
    if page.load_error().is_some() {
        BackendHealth::Unavailable
    } else if page.loading() || page.snapshot().is_none() {
        BackendHealth::Unknown
    } else if page.partial()
        || page.snapshot().is_some_and(|snapshot| {
            snapshot.status().is_some_and(|status| {
                status
                    .services()
                    .iter()
                    .any(|service| !matches!(service.state(), ServiceState::Active))
            })
        })
    {
        BackendHealth::Degraded
    } else {
        BackendHealth::Healthy
    }
}

fn partial_failure_presentation(error: &crate::ports::SystemConfiguratorError) -> (String, String) {
    match error {
        crate::ports::SystemConfiguratorError::Cancelled => (
            gettextrs::gettext("Apply interrupted"),
            gettextrs::gettext(
                "Apply was cancelled after some operations completed. Persisted values were read back.",
            ),
        ),
        crate::ports::SystemConfiguratorError::AuthorizationDenied { message, .. } => (
            gettextrs::gettext("Authorization denied"),
            message.clone(),
        ),
        crate::ports::SystemConfiguratorError::ValuesRejected { message, .. } => (
            gettextrs::gettext("Backend rejected changes"),
            message.clone(),
        ),
        crate::ports::SystemConfiguratorError::Execution { message, .. } => {
            (gettextrs::gettext("Apply failed"), message.clone())
        }
    }
}

/// Redact-and-format the full details for a privileged failure that carries
/// the executable/argv/exit-status/stderr/message the adapter retained. Every
/// field flows through [`diagnostics::redact_text`] before being rendered.
fn privileged_failure_details(details: &crate::backend_apply::PrivilegedFailure) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext("Executable:"));
    out.push(' ');
    out.push_str(&diagnostics::redact_text(details.executable()));
    out.push('\n');
    out.push_str(&gettextrs::gettext("Arguments:"));
    if details.arguments().is_empty() {
        out.push_str(" —");
    } else {
        for argument in details.arguments() {
            out.push(' ');
            out.push_str(&diagnostics::redact_text(argument));
        }
    }
    out.push('\n');
    out.push_str(&gettextrs::gettext("Exit status:"));
    out.push(' ');
    match details.exit_status() {
        Some(code) => out.push_str(&code.to_string()),
        None => out.push('—'),
    }
    out.push('\n');
    if !details.stderr().trim().is_empty() {
        out.push_str(&gettextrs::gettext("Standard error:"));
        out.push('\n');
        out.push_str(&diagnostics::redact_text(details.stderr()));
        out.push('\n');
    }
    out.push_str(&gettextrs::gettext("Message:"));
    out.push('\n');
    out.push_str(&diagnostics::redact_text(details.message()));
    out
}

/// Format the executable, argv, exit status, stderr, and message retained on
/// a [`SystemConfiguratorError`]. Every field flows through
/// [`diagnostics::redact_text`] first.
fn system_error_details(error: &crate::ports::SystemConfiguratorError) -> String {
    let (executable, arguments, exit_status, stderr, message) = match error {
        crate::ports::SystemConfiguratorError::Cancelled => (
            "",
            &[] as &[String],
            None,
            "",
            "privileged configuration was cancelled",
        ),
        crate::ports::SystemConfiguratorError::AuthorizationDenied {
            executable,
            arguments,
            exit_status,
            stderr,
            message,
        }
        | crate::ports::SystemConfiguratorError::ValuesRejected {
            executable,
            arguments,
            exit_status,
            stderr,
            message,
        }
        | crate::ports::SystemConfiguratorError::Execution {
            executable,
            arguments,
            exit_status,
            stderr,
            message,
        } => (
            executable.as_str(),
            arguments.as_slice(),
            *exit_status,
            stderr.as_str(),
            message.as_str(),
        ),
    };
    let mut out = String::new();
    out.push_str(&gettextrs::gettext("Executable:"));
    out.push(' ');
    if executable.is_empty() {
        out.push('—');
    } else {
        out.push_str(&diagnostics::redact_text(executable));
    }
    out.push('\n');
    out.push_str(&gettextrs::gettext("Arguments:"));
    if arguments.is_empty() {
        out.push_str(" —");
    } else {
        for argument in arguments {
            out.push(' ');
            out.push_str(&diagnostics::redact_text(argument));
        }
    }
    out.push('\n');
    out.push_str(&gettextrs::gettext("Exit status:"));
    out.push(' ');
    match exit_status {
        Some(code) => out.push_str(&code.to_string()),
        None => out.push('—'),
    }
    out.push('\n');
    if !stderr.trim().is_empty() {
        out.push_str(&gettextrs::gettext("Standard error:"));
        out.push('\n');
        out.push_str(&diagnostics::redact_text(stderr));
        out.push('\n');
    }
    out.push_str(&gettextrs::gettext("Message:"));
    out.push('\n');
    out.push_str(&diagnostics::redact_text(message));
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
                    .unwrap_or_else(|| "—".to_owned()),
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
        gettextrs::gettext("Backend:"),
        diagnostics::redact_text(snapshot.identity().snap_name()),
    ));
    if let Some(status) = snapshot.status() {
        let services = status.services();
        if services.is_empty() {
            out.push_str(&format!("  {}\n", gettextrs::gettext("Services: —"),));
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
                "    {:?}: {}\n",
                surface,
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
    let stripped = snap_name.strip_prefix("myna-").unwrap_or(snap_name);
    let mut title = String::with_capacity(stripped.len());
    let mut capitalize = true;
    for ch in stripped.chars() {
        if ch == '-' || ch == '_' {
            title.push(' ');
            capitalize = true;
        } else if capitalize {
            for upper in ch.to_uppercase() {
                title.push(upper);
            }
            capitalize = false;
        } else {
            title.push(ch);
        }
    }
    title
}

fn setting_widget_name(key: &str) -> String {
    format!("myna-setting-{}", key.replace('.', "-"))
}

/// Widget name of the staged-changes group, so it can be replaced in place.
const STAGED_CHANGES_GROUP: &str = "myna-staged-changes";

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

fn backend_to_cancel<'a>(previous: &'a Selection, next: &Selection) -> Option<&'a str> {
    match previous {
        Selection::Backend(name) if !matches!(next, Selection::Backend(next_name) if next_name == name) => {
            Some(name)
        }
        _ => None,
    }
}

fn update_backend_row(row: &ui::SidebarRow, page: &BackendPage) {
    let short = page.short_status();
    row.set_title(&display_title_for(page.identity().snap_name()));
    row.set_subtitle(&escape_markup(&subtitle_for(&short)));
    let mut description = format!(
        "{}. {}",
        gettextrs::gettext("Backend"),
        subtitle_for(&short)
    );
    if page.partial() {
        description.push(' ');
        description.push_str(&gettextrs::gettext("Some information could not be read."));
    }
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Description(&description)]);
}

fn subtitle_for(status: &crate::backend_controller::BackendShortStatus) -> String {
    let mut parts = Vec::new();
    parts.push(
        match status.connection {
            ConnectionKind::Active => gettextrs::gettext("Connected"),
            ConnectionKind::Contested => gettextrs::gettext("Multiple connections"),
            ConnectionKind::Disconnected => gettextrs::gettext("Not connected"),
        }
        .to_owned(),
    );
    if status.loading {
        parts.push(gettextrs::gettext("Refreshing…"));
    } else if status.unavailable {
        parts.push(gettextrs::gettext("Unavailable"));
    } else if status.partial {
        parts.push(gettextrs::gettext("Partial data"));
    }
    if let Some(model) = &status.active_model {
        parts.push(model.clone());
    } else if let Some(engine) = &status.active_engine {
        parts.push(engine.clone());
    }
    parts.join(" · ")
}

fn build_backend_page(page: &BackendPage, ui: &Rc<BackendUi>) -> adw::NavigationPage {
    let page_widget = ui::BackendPage::new();
    let preferences = page_widget.preferences_page();
    let title = display_title_for(page.identity().snap_name());
    page_widget.set_display_title(&title);

    let overview = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Overview"))
        .description(escape_markup(&overview_description(page)))
        .build();
    let short = page.short_status();
    let health = adw::ActionRow::builder()
        .title(gettextrs::gettext("Connection"))
        .subtitle(escape_markup(&match short.connection {
            ConnectionKind::Active => gettextrs::gettext("Active — this backend serves Myna."),
            ConnectionKind::Contested => gettextrs::gettext(
                "Multiple backends are connected. Only one should be active at a time.",
            ),
            ConnectionKind::Disconnected => gettextrs::gettext("Not connected to the Myna daemon."),
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
            None => gettextrs::gettext("None selected — choose one below."),
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
    preferences.add(&overview);

    if let Some(message) = page.load_error() {
        let unavailable = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Unavailable"))
            .description(escape_markup(message))
            .build();
        preferences.add(&unavailable);
    } else if page.loading() && page.snapshot().is_none() {
        let loading = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Loading"))
            .description(gettextrs::gettext(
                "Reading configuration from this backend…",
            ))
            .build();
        preferences.add(&loading);
    } else if let Some(snapshot) = page.snapshot() {
        if snapshot.configuration().is_empty()
            && snapshot.models().is_none()
            && snapshot.engines().is_none()
        {
            let empty = adw::PreferencesGroup::builder()
                .title(gettextrs::gettext("No configuration reported"))
                .description(gettextrs::gettext(
                    "The backend accepted requests but reported no configurable settings.",
                ))
                .build();
            preferences.add(&empty);
        }
        add_configuration_groups(&preferences, page, ui);
    }

    if page.partial() {
        let details = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Some data is unavailable"))
            .description(gettextrs::gettext(
                "Raw failure details are recorded on the Diagnostics page.",
            ))
            .build();
        preferences.add(&details);
    }

    let applying = ui.apply_state_view(page.identity().snap_name()).in_progress;
    let (refresh_enabled, refresh_label) = refresh_control_state(page.loading(), applying);
    let refresh = page_widget.refresh_button();
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

    page_widget.upcast()
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

fn add_configuration_groups(
    page_widget: &adw::PreferencesPage,
    page: &BackendPage,
    ui: &Rc<BackendUi>,
) {
    if page.rows().is_empty() {
        add_apply_group(page_widget, page, ui);
        return;
    }
    let mut groups: BTreeMap<crate::presentation::PresentationGroup, adw::PreferencesGroup> =
        BTreeMap::new();
    for row in page.rows() {
        if row.presentation().metadata().diagnostics_only() {
            continue;
        }
        let key = row.presentation().metadata().group();
        let group = groups.entry(key).or_insert_with(|| {
            adw::PreferencesGroup::builder()
                .title(group_title(key))
                .description(group_description(key))
                .build()
        });
        group.add(&build_row_widget(row, ui, page));
    }
    for (_, group) in groups {
        page_widget.add(&group);
    }
    add_apply_group(page_widget, page, ui);
}

fn add_apply_group(page_widget: &adw::PreferencesPage, page: &BackendPage, ui: &Rc<BackendUi>) {
    let view = ui.apply_state_view(page.identity().snap_name());
    let validation = match prepare_backend_apply(page) {
        Ok(_) | Err(PrepareApplyError::NoChanges) => None,
        Err(PrepareApplyError::Invalid(issues)) => Some(issues),
    };
    let has_content = view.in_progress
        || view.feedback.is_some()
        || !page.dirty_keys().is_empty()
        || validation.is_some();
    if !has_content {
        return;
    }

    let description = if let Some(message) = view.progress_message.as_deref() {
        message.to_owned()
    } else if let Some(feedback) = &view.feedback {
        feedback.description.clone()
    } else if validation.is_some() {
        gettextrs::gettext("Fix invalid values before applying these backend changes.")
    } else {
        gettextrs::gettext(
            "Review staged changes, authorize one privileged operation, then verify read-back.",
        )
    };
    let group = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Staged changes"))
        .description(escape_markup(&description))
        .build();
    group.set_widget_name(STAGED_CHANGES_GROUP);

    if let Some(feedback) = &view.feedback {
        let row = adw::ActionRow::builder()
            .title(escape_markup(&feedback.title))
            .subtitle(escape_markup(&feedback.description))
            .build();
        row.set_activatable(false);
        group.add(&row);
    }

    for row in page.rows().iter().filter(|row| row.dirty()) {
        let diff = adw::ActionRow::builder()
            .title(escape_markup(row.presentation().metadata().title()))
            .subtitle(escape_markup(&format!(
                "{} → {}",
                config_value_display(row.presentation().value()),
                config_value_display(row.effective_value())
            )))
            .build();
        diff.set_activatable(false);
        group.add(&diff);
    }

    if let Some(issues) = &validation {
        for issue in issues {
            let row = adw::ActionRow::builder()
                .title(escape_markup(issue.title()))
                .subtitle(escape_markup(issue.message()))
                .build();
            row.set_activatable(false);
            group.add(&row);
        }
    }

    let controls = ui::BackendApplyControls::new();
    let controls_subtitle = if view.in_progress {
        view.progress_message
            .clone()
            .unwrap_or_else(|| gettextrs::gettext("Applying backend changes…"))
    } else {
        gettextrs::gettext("A single authorization applies all staged changes together.")
    };
    controls.set_subtitle(&escape_markup(&controls_subtitle));
    if view.cancellable {
        let spinner = controls.progress_spinner();
        spinner.set_visible(true);
        spinner.start();
    }

    let button_box = controls.button_box();
    let revert = controls.revert_button();
    revert.set_sensitive(!page.dirty_keys().is_empty() && !view.in_progress);
    revert.connect_clicked({
        let controller = Rc::downgrade(&ui.controller);
        let snap = page.identity().snap_name().to_owned();
        let ui = Rc::downgrade(ui);
        move |_| {
            if let Some(controller) = controller.upgrade() {
                controller.revert_all_edits(&snap);
            }
            if let Some(ui) = ui.upgrade() {
                ui.clear_apply_feedback(&snap);
                ui.rebuild_backend_page(&snap);
            }
        }
    });
    let apply = controls.apply_button();
    let another_operation_active = ui.operation_coordinator.active().is_some() && !view.in_progress;
    apply.set_sensitive(
        !page.dirty_keys().is_empty()
            && !view.in_progress
            && !another_operation_active
            && validation.is_none(),
    );
    apply.connect_clicked({
        let ui = Rc::downgrade(ui);
        let snap = page.identity().snap_name().to_owned();
        move |_| {
            if let Some(ui) = ui.upgrade() {
                ui.begin_apply(&snap);
            }
        }
    });
    if view.cancellable {
        let cancel = gtk::Button::with_label(&gettextrs::gettext("Cancel"));
        cancel.connect_clicked({
            let ui = Rc::downgrade(ui);
            let snap = page.identity().snap_name().to_owned();
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.cancel_apply(&snap);
                }
            }
        });
        button_box.append(&cancel);
    }

    group.add(&controls);
    page_widget.add(&group);
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
            gettextrs::gettext("Common backend preferences.")
        }
        crate::presentation::PresentationGroup::Runtime => {
            gettextrs::gettext("Runtime and performance related settings.")
        }
        crate::presentation::PresentationGroup::Advanced => gettextrs::gettext(
            "Advanced values. The backend supplies no richer metadata for these keys.",
        ),
        crate::presentation::PresentationGroup::Sensitive => {
            gettextrs::gettext("Internal and sensitive values are shown for diagnostics only.")
        }
    }
}

fn build_row_widget(row: &BackendRow, ui: &Rc<BackendUi>, page: &BackendPage) -> gtk::Widget {
    let controller = &ui.controller;
    let snap = page.identity().snap_name().to_owned();
    let key = row.presentation().key().to_owned();
    let metadata = row.presentation().metadata();
    let title = metadata.title();
    let description = metadata.explanation();
    let editable = !ui.apply_state_view(page.identity().snap_name()).in_progress;
    if metadata.diagnostics_only() {
        let value = if metadata.sensitivity() == Sensitivity::Sensitive {
            gettextrs::gettext("Sensitive value (redacted)")
        } else {
            diagnostics::redact_text(&config_value_display(row.effective_value()))
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
    match metadata.control() {
        ControlType::Toggle => {
            let switch = adw::SwitchRow::builder()
                .title(title)
                .subtitle(description)
                .active(matches!(row.effective_value(), ConfigValue::Boolean(true)))
                .build();
            switch.set_widget_name(&setting_widget_name(&key));
            switch
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            switch.set_sensitive(editable);
            let controller_weak = Rc::downgrade(controller);
            let updating = Rc::new(std::cell::Cell::new(false));
            switch.connect_active_notify({
                let controller_weak = controller_weak.clone();
                let snap = snap.clone();
                let key = key.clone();
                let updating = updating.clone();
                move |row| {
                    if updating.get() {
                        return;
                    }
                    if let Some(controller) = controller_weak.upgrade() {
                        controller.stage_edit(&snap, &key, ConfigValue::Boolean(row.is_active()));
                    }
                }
            });
            switch.upcast()
        }
        ControlType::Choice => {
            let choices = row.presentation().choices();
            let model =
                gtk::StringList::new(&choices.iter().map(String::as_str).collect::<Vec<_>>());
            let combo = adw::ComboRow::builder()
                .title(title)
                .subtitle(description)
                .model(&model)
                .build();
            combo.set_widget_name(&setting_widget_name(&key));
            combo
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            combo.set_sensitive(editable);
            if let ConfigValue::Text(current) = row.effective_value() {
                if let Some(index) = choices.iter().position(|choice| choice == current) {
                    combo.set_selected(index as u32);
                }
            }
            let controller_weak = Rc::downgrade(controller);
            let choices_owned = choices.to_vec();
            combo.connect_selected_notify({
                let controller_weak = controller_weak.clone();
                let snap = snap.clone();
                let key = key.clone();
                move |row| {
                    if let Some(controller) = controller_weak.upgrade() {
                        if let Some(choice) = choices_owned.get(row.selected() as usize) {
                            controller.stage_edit(&snap, &key, ConfigValue::Text(choice.clone()));
                        }
                    }
                }
            });
            combo.upcast()
        }
        ControlType::ReadOnly => {
            let action = adw::ActionRow::builder()
                .title(title)
                .subtitle(escape_markup(&config_value_display(row.effective_value())))
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
                .text(config_value_display(row.effective_value()))
                .show_apply_button(true)
                .build();
            entry.set_widget_name(&setting_widget_name(&key));
            entry.set_tooltip_text(Some(description));
            entry
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description(description)]);
            entry.set_sensitive(editable);
            let controller_weak = Rc::downgrade(controller);
            let control = metadata.control();
            entry.connect_changed({
                let controller_weak = controller_weak.clone();
                let snap = snap.clone();
                let key = key.clone();
                move |row| {
                    if let Some(controller) = controller_weak.upgrade() {
                        controller.stage_edit(
                            &snap,
                            &key,
                            parse_editable_value(control, row.text().as_str()),
                        );
                    }
                }
            });
            entry.connect_apply({
                let controller_weak = controller_weak.clone();
                let snap = snap.clone();
                let key = key.clone();
                move |row| {
                    if let Some(controller) = controller_weak.upgrade() {
                        let raw = row.text().to_string();
                        let value = parse_editable_value(control, &raw);
                        controller.stage_edit(&snap, &key, value);
                    }
                }
            });
            entry.upcast()
        }
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
        BackendSurface::Connections => gettextrs::gettext("Backend connections"),
        BackendSurface::ModelctlApp => gettextrs::gettext("Model control command"),
        BackendSurface::ModelctlConfig => gettextrs::gettext("Backend configuration"),
        BackendSurface::Status => gettextrs::gettext("Backend status"),
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
        (
            false,
            gettextrs::gettext("Applying backend changes and verifying restart…"),
        )
    } else if loading {
        (false, gettextrs::gettext("Refreshing backend data…"))
    } else {
        (true, gettextrs::gettext("Refresh backend data"))
    }
}

fn apply_progress_message(restart_impact: crate::backend_apply::RestartImpact) -> String {
    if restart_impact.requires_readiness() {
        gettextrs::gettext("Applying backend changes and waiting for restart/readiness…")
    } else {
        gettextrs::gettext("Applying backend changes and verifying read-back…")
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
            format!(
                "{} requested {}, read back {}",
                mismatch.key(),
                config_value_display(mismatch.requested()),
                actual
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Redact and format the full details for a failed backend switch operation.
/// Includes:
///
/// * a short reconciliation header describing whether the final snapshot
///   agrees with the requested change,
/// * every completed command (executable + argv), and
/// * the primary error message.
///
/// All fields go through [`diagnostics::redact_text`] so no absolute paths or
/// secret-looking values reach the dialog.
fn switch_failure_details(
    error: &crate::ports::SystemConfiguratorError,
    discovery_error: Option<&crate::domain::BackendSurfaceError>,
    completed: &[crate::domain::CommandResult],
    final_snapshot: Option<&crate::domain::ConnectionSnapshot>,
) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext("Backend switch failed."));
    out.push('\n');
    match final_snapshot {
        Some(snapshot) => {
            out.push_str(&format!(
                "{} {}\n",
                gettextrs::gettext("Final connections:"),
                connection_state_summary(snapshot)
            ));
        }
        None => {
            if let Some(discovery_error) = discovery_error {
                out.push_str(&format!(
                    "{} {}\n",
                    gettextrs::gettext("Final connections could not be verified:"),
                    diagnostics::redact_text(discovery_error.message())
                ));
            } else {
                out.push_str(&gettextrs::gettext(
                    "Final connections could not be verified.\n",
                ));
            }
        }
    }
    out.push('\n');
    if completed.is_empty() {
        out.push_str(&gettextrs::gettext("No snapd operations completed.\n"));
    } else {
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
                    .unwrap_or_else(|| "—".to_owned()),
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
    out.push_str(&gettextrs::gettext("Error:\n"));
    out.push_str(&system_error_details(error));
    out
}

fn final_discovery_details(
    error: &crate::domain::BackendSurfaceError,
    completed: &[crate::domain::CommandResult],
) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext(
        "Backend switch succeeded, but the final connection state could not be verified.\n\n",
    ));
    if !completed.is_empty() {
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
                    .unwrap_or_else(|| "—".to_owned()),
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
    out.push_str(&gettextrs::gettext("Discovery error:\n"));
    out.push_str(&diagnostics::redact_text(error.message()));
    out
}

fn connection_state_summary(snapshot: &crate::domain::ConnectionSnapshot) -> String {
    match snapshot.active_state() {
        crate::domain::ActiveBackendState::Disconnected => {
            gettextrs::gettext("no backend connected")
        }
        crate::domain::ActiveBackendState::Connected(backend) => {
            format!(
                "{} — {}",
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
            format!("{}: {}", gettextrs::gettext("multiple backends"), names)
        }
        crate::domain::ActiveBackendState::FailedSwitch { .. } => {
            gettextrs::gettext("previous switch is unresolved")
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

/// Whether `staged` is the display form of the value `typed` stages, so a
/// rebuilt entry showing `staged` may show `typed` instead. Half-typed numbers
/// such as "0." stage as 0 and would otherwise lose their last keystroke.
fn typed_text_stages_as(typed: &str, staged: &str) -> bool {
    config_value_display(&parse_editable_value(ControlType::Number, typed)) == staged
}

fn config_value_display(value: &ConfigValue) -> String {
    match value {
        ConfigValue::Null => String::new(),
        ConfigValue::Boolean(true) => "true".to_owned(),
        ConfigValue::Boolean(false) => "false".to_owned(),
        ConfigValue::Integer(number) => number.to_string(),
        ConfigValue::Number(number) => number.to_string(),
        ConfigValue::Text(text) => text.clone(),
        ConfigValue::List(items) => items
            .iter()
            .map(config_value_display)
            .collect::<Vec<_>>()
            .join(", "),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn display_title_strips_prefix_and_capitalizes() {
        assert_eq!(display_title_for("myna-parakeet"), "Parakeet");
        assert_eq!(display_title_for("myna-whisper-small"), "Whisper Small");
        assert_eq!(display_title_for("custom"), "Custom");
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
    fn typed_text_is_kept_only_when_the_rebuilt_text_is_its_staged_form() {
        assert!(typed_text_stages_as("0.", "0"));
        assert!(typed_text_stages_as("0.50", "0.5"));
        assert!(typed_text_stages_as("free text", "free text"));
        assert!(!typed_text_stages_as("0.2", "0.3"));
        assert!(!typed_text_stages_as("0.", "0.5"));
    }

    #[test]
    fn subtitle_summarises_connection_and_extras() {
        let status = crate::backend_controller::BackendShortStatus {
            connection: ConnectionKind::Active,
            active_model: Some("parakeet".to_owned()),
            active_engine: None,
            loading: false,
            partial: false,
            unavailable: false,
        };
        let subtitle = subtitle_for(&status);
        assert!(subtitle.contains("parakeet"));
    }

    #[test]
    fn refresh_control_is_pending_and_disabled_while_loading() {
        assert_eq!(
            refresh_control_state(true, false),
            (false, gettextrs::gettext("Refreshing backend data…"))
        );
        assert_eq!(
            refresh_control_state(false, false),
            (true, gettextrs::gettext("Refresh backend data"))
        );
        assert_eq!(
            refresh_control_state(false, true),
            (
                false,
                gettextrs::gettext("Applying backend changes and verifying restart…")
            )
        );
    }

    #[test]
    fn replacing_a_selected_page_does_not_cancel_its_refresh() {
        let backend = Selection::Backend("myna-parakeet".to_owned());
        assert_eq!(backend_to_cancel(&backend, &backend), None);
        assert_eq!(
            backend_to_cancel(&backend, &Selection::Diagnostics),
            Some("myna-parakeet")
        );
    }

    struct TestUi {
        ui: Rc<BackendUi>,
        myna_row: gtk::ListBoxRow,
        diagnostics_row: gtk::ListBoxRow,
    }

    fn test_ui(controller: Rc<BackendController>) -> TestUi {
        let split_view = adw::NavigationSplitView::new();
        let sidebar_list = gtk::ListBox::new();
        let myna_row = gtk::ListBoxRow::new();
        let diagnostics_row = gtk::ListBoxRow::new();
        sidebar_list.append(&myna_row);
        sidebar_list.append(&diagnostics_row);
        let page = |title: &str| {
            adw::NavigationPage::builder()
                .title(title)
                .child(&gtk::Label::new(Some(title)))
                .build()
        };
        let operation_coordinator = OperationCoordinator::new();
        let ui = Rc::new(BackendUi {
            controller,
            configurator: Rc::new(PkexecSystemConfigurator::new(Arc::new(GioCommandRunner))),
            sidebar_list,
            split_view,
            overlay: adw::ToastOverlay::new(),
            myna_row: myna_row.clone(),
            myna_page: RefCell::new(Some(page("Myna"))),
            myna_selector: None,
            active_backend: ActiveBackendController::with_coordinator(
                crate::domain::ConnectionSnapshot::new(
                    Vec::new(),
                    ActiveBackendState::Disconnected,
                ),
                operation_coordinator.clone(),
            ),
            operation_coordinator,
            diagnostics_row: diagnostics_row.clone(),
            diagnostics_page: RefCell::new(Some(page("Diagnostics"))),
            backend_rows: RefCell::new(BTreeMap::new()),
            backend_pages: RefCell::new(BTreeMap::new()),
            apply_state: RefCell::new(BTreeMap::new()),
            selected: RefCell::new(Selection::Myna),
            installed_snaps: RefCell::new(Vec::new()),
            inventory_complete: std::cell::Cell::new(false),
            inventory_failure: RefCell::new(None),
            performance: RefCell::new(None),
            last_diagnostics_refresh: std::cell::Cell::new(None),
        });
        ui.connect_sidebar_selection();
        TestUi {
            ui,
            myna_row,
            diagnostics_row,
        }
    }

    #[test]
    fn every_valid_sidebar_selection_reveals_content() {
        if gtk::init().is_err() {
            return;
        }

        let TestUi {
            ui,
            myna_row,
            diagnostics_row,
        } = test_ui(BackendController::detached());

        for row in [&diagnostics_row, &myna_row] {
            ui.split_view.set_show_content(false);
            ui.sidebar_list.select_row(Some(row));
            assert!(ui.split_view.shows_content());
        }
    }

    /// A connected parakeet backend whose only setting is the pause-length
    /// number, shown on screen with the entry row focused as a user typing
    /// into it would have it.
    fn focused_pause_length_entry() -> Option<(Rc<BackendUi>, gtk::Window)> {
        if gtk::init().is_err() {
            return None;
        }
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        let connections = crate::domain::parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend myna-parakeet:provider manual\n",
            "name: content\n",
        )
        .expect("connections parse");
        controller.complete_discovery(request, Ok(connections));
        let request = controller.begin_snapshot("myna-parakeet").unwrap();
        let mut snapshot = crate::domain::BackendSnapshot::empty(BackendIdentity::new(
            "myna-parakeet",
            "provider",
        ));
        snapshot.set_modelctl_config(
            crate::domain::parse_modelctl_config("stream-silence-cut-seconds: 0.5\n")
                .expect("modelctl parse"),
        );
        controller.complete_snapshot(request, snapshot);

        let TestUi { ui, .. } = test_ui(controller);
        ui.controller.observe({
            let ui = Rc::downgrade(&ui);
            move |event| {
                if let Some(ui) = ui.upgrade() {
                    ui.on_controller_event(event);
                }
            }
        });
        let window = gtk::Window::builder().child(&ui.split_view).build();
        let selection = Selection::Backend("myna-parakeet".to_owned());
        *ui.selected.borrow_mut() = selection.clone();
        ui.show_selection(&selection);
        pause_length_entry(&ui).grab_focus();
        Some((ui, window))
    }

    fn pause_length_entry(ui: &BackendUi) -> adw::EntryRow {
        let page = ui.split_view.content().expect("backend page shown");
        find_named_descendant(page.upcast_ref(), "myna-setting-stream-silence-cut-seconds")
            .expect("pause length entry")
            .downcast()
            .expect("entry row")
    }

    fn staged_pause_length(ui: &BackendUi) -> Option<ConfigValue> {
        let page = ui.controller.page("myna-parakeet")?;
        page.rows()
            .iter()
            .find(|row| row.presentation().key() == "stream-silence-cut-seconds")
            .filter(|row| row.dirty())
            .map(|row| row.effective_value().clone())
    }

    #[test]
    fn typing_a_fraction_into_a_number_entry_keeps_every_keystroke() {
        let Some((ui, _window)) = focused_pause_length_entry() else {
            return;
        };

        // Typing "0.2" over the default: the first two keystrokes are not yet
        // a valid setting, and the rebuild each one triggers must not rewrite
        // the text the user is still typing.
        for (typed, staged) in [
            ("0", ConfigValue::Integer(0)),
            ("0.", ConfigValue::Number(0.0)),
            ("0.2", ConfigValue::Number(0.2)),
        ] {
            let entry = pause_length_entry(&ui);
            entry.set_text(typed);
            assert_eq!(
                staged_pause_length(&ui),
                Some(staged),
                "staged after {typed:?}"
            );
            assert_eq!(pause_length_entry(&ui).text().as_str(), typed);
        }
    }

    #[test]
    fn a_surface_failure_becomes_one_sentence_with_no_command_line() {
        let error = crate::domain::BackendSurfaceError::new(
            crate::domain::BackendSurface::Connections,
            "snap",
            ["connections", "--all"].map(str::to_owned).to_vec(),
            "snap connections failed",
            "permission denied",
        );

        let problem = problem_from_surface_error(&error);
        assert_eq!(problem, "Backend connections: snap connections failed");
    }

    #[test]
    fn readback_failure_details_are_complete_copyable_and_privacy_safe() {
        let errors = [
            crate::domain::BackendSurfaceError::new(
                crate::domain::BackendSurface::ModelctlConfig,
                "snap",
                ["run", "myna-whisper.whisper", "get"]
                    .map(str::to_owned)
                    .to_vec(),
                "command exited unsuccessfully with status Some(1)",
                "private config output",
            ),
            crate::domain::BackendSurfaceError::new(
                crate::domain::BackendSurface::Models,
                "snap",
                ["run", "myna-whisper.whisper", "list-models"]
                    .map(str::to_owned)
                    .to_vec(),
                "model list unavailable",
                "",
            ),
        ];

        let details = read_back_failure_details(&errors);
        assert!(details.contains("command exited unsuccessfully with status Some(1)"));
        assert!(details.contains("model list unavailable"));
        assert!(!details.contains("private config output"));
        assert!(!details.contains("snap run"));
    }

    #[test]
    fn disappearing_backend_signals_apply_but_retains_gate_until_completion() {
        let coordinator = OperationCoordinator::new();
        let operation = coordinator.begin(OperationKind::BackendApply).unwrap();
        let token = operation.cancellation();
        let mut state = BTreeMap::from([(
            "myna-parakeet".to_owned(),
            BackendApplyState {
                confirmation_pending: false,
                operation_token: Some(operation.token()),
                operation_cancellation: Some(token.clone()),
                progress_message: Some("Applying…".to_owned()),
                cancellation: Some(token.clone()),
                feedback: None,
            },
        )]);

        remove_apply_state(&mut state, &coordinator, "myna-parakeet");

        assert!(token.is_cancelled());
        assert_eq!(
            state
                .get("myna-parakeet")
                .and_then(|entry| entry.operation_token),
            Some(operation.token())
        );
        assert!(coordinator.begin(OperationKind::BackendSwitch).is_err());
        assert!(coordinator.complete(operation.token()));
        assert!(coordinator.begin(OperationKind::BackendSwitch).is_ok());
    }

    #[test]
    fn final_teardown_abandons_active_apply_and_clears_progress() {
        let coordinator = OperationCoordinator::new();
        let operation = coordinator.begin(OperationKind::BackendApply).unwrap();
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        let mut state = BTreeMap::from([
            (
                "myna-parakeet".to_owned(),
                BackendApplyState {
                    confirmation_pending: false,
                    operation_token: Some(operation.token()),
                    operation_cancellation: None,
                    progress_message: Some("Applying…".to_owned()),
                    cancellation: Some(first.clone()),
                    feedback: None,
                },
            ),
            (
                "myna-whisper".to_owned(),
                BackendApplyState {
                    confirmation_pending: false,
                    operation_token: None,
                    operation_cancellation: None,
                    progress_message: Some("Applying…".to_owned()),
                    cancellation: Some(second.clone()),
                    feedback: None,
                },
            ),
        ]);

        abandon_all_apply_state(&mut state, &coordinator);

        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
        assert!(state.values().all(|entry| entry.cancellation.is_none()));
        assert_eq!(coordinator.active(), None);
    }
}
