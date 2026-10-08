//! GTK/libadwaita wiring for the onboarding wizard.
//!
//! Thin, like [`crate::backend_ui`]: [`crate::onboarding`] decides what is
//! missing, how to install it, and when the flow may advance; this module
//! renders that, connects and restarts what the user installed, and closes
//! when the user is done.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::active_backend::{ensure_backend_active, Settled, SetupError, SetupStage, SnapdWait};
use crate::adapters::shell_extensions::GnomeShellExtensions;
use crate::adapters::snap_backend::SnapBackendRepository;
use crate::adapters::system_configurator::PkexecSystemConfigurator;
use crate::command::{CancellationToken, GioCommandRunner};
use crate::domain::BackendSurfaceError;
use crate::onboarding::{
    assess, can_advance, completes, failed_set_up_step, flag_enabled, forward_leads, install_view,
    installs, model_offer, needs_onboarding, next_install, polls, relogin_pending,
    remaining_download, set_up_plan, set_up_steps, settled, while_installing, Component,
    ComponentId, ComponentState, DownloadSize, InstallView, Machine, ModelOffer, Step,
    RECOMMENDED_BACKEND_SNAP, SHELL_EXTENSION_UUID,
};
use crate::ports::{
    BackendRepository, ShellExtensions, SystemConfigurator, SystemConfiguratorError,
};
use crate::shortcut_ui::set_class;
use crate::snap_changes::{pending_install, ApplyProgress};
use crate::snap_install::{follow_change, Follow};
use crate::ui;

/// How often the component step re-reads the machine while something is
/// missing: one `snap list` and one discovery each time.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long setting up waits for snapd to finish installing Myna or a model,
/// which may still be downloading it.
const SNAPD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long "All required components installed" shows before the wizard moves on by
/// itself.
const BEAT: Duration = Duration::from_secs(1);
/// How long setting up runs before its spinner shows. Restarting the daemon
/// alone takes a fraction of this, and a spinner flashing past reads as a
/// glitch.
const SPINNER_DELAY: Duration = Duration::from_secs(1);
/// How long setting up waits for the restarted daemon to claim its bus
/// name, about 0.4 s on Noble, before moving on regardless.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(10);
/// How often an install's change is read for its progress.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(1);

/// Where a row's install stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Install {
    /// snapd is installing it: its polkit prompt may still be open, and the
    /// percentage appears once a download announces its size.
    Running(Option<u8>),
    /// snapd is done; the row waits for a read that shows it.
    Confirming,
}

/// Where the wizard was opened from, which decides what Done closes.
#[derive(Clone, Copy)]
pub enum Opener<'a> {
    /// First run: Done closes Myna Settings.
    FirstRun,
    /// The settings window's menu: modal over it, and Done closes only the
    /// wizard.
    Settings(&'a gtk::Window),
}

pub struct OnboardingUi {
    window: ui::OnboardingWindow,
    /// Done closes only the wizard, leaving the settings window it opened
    /// from.
    keeps_settings: bool,
    components_page: ui::OnboardingComponents,
    shortcut_page: ui::OnboardingShortcut,
    shortcut: Rc<crate::shortcut_ui::ShortcutControl>,
    repository: Rc<dyn BackendRepository>,
    configurator: Rc<dyn SystemConfigurator>,
    extensions: Rc<dyn ShellExtensions>,
    step: Cell<Step>,
    components: RefCell<Vec<Component>>,
    offer: Cell<ModelOffer>,
    /// Why the last assessment could not read the machine.
    problem: RefCell<Option<String>>,
    busy: Cell<bool>,
    /// The button's run is installing what is missing, one step at a time.
    running: Cell<bool>,
    /// The run's current step.
    run_step: Cell<Option<ComponentId>>,
    /// Bumped by every install step, so a read that started before one is
    /// not taken for the machine after it.
    epoch: Cell<u64>,
    installs: RefCell<BTreeMap<ComponentId, Install>>,
    /// Stop following installs when the wizard closes; snapd carries on.
    install_cancellation: CancellationToken,
    follow_interval: Cell<Duration>,
    stage: RefCell<Option<SetupStage>>,
    setup_cancellation: RefCell<Option<CancellationToken>>,
    /// Setting up has run past [`SPINNER_DELAY`], so the footer says so.
    setup_slow: Cell<bool>,
    /// The last setup did not leave dictation running.
    setup_failed: Cell<bool>,
    spinner_timer: RefCell<Option<glib::SourceId>>,
    /// The last line logged, so a poll repeats none.
    logged: RefCell<String>,
    assessing: Cell<bool>,
    poll_interval: Cell<Duration>,
    poll: RefCell<Option<glib::SourceId>>,
    beat_length: Cell<Duration>,
    beat: RefCell<Option<glib::SourceId>>,
}

impl OnboardingUi {
    /// Build and present the wizard against the real snapd and snap ports.
    pub fn present(application: &adw::Application, initial: Vec<Component>) -> Rc<Self> {
        let runner = Arc::new(GioCommandRunner);
        Self::present_with_ports(
            application,
            initial,
            Rc::new(SnapBackendRepository::new(runner.clone())),
            Rc::new(PkexecSystemConfigurator::new(runner)),
            Rc::new(GnomeShellExtensions::new()),
            Opener::FirstRun,
        )
    }

    /// Opened from the settings window, the wizard is modal over it: its
    /// own operations cannot start while the wizard sets a backend up.
    pub fn present_with_ports(
        application: &adw::Application,
        initial: Vec<Component>,
        repository: Rc<dyn BackendRepository>,
        configurator: Rc<dyn SystemConfigurator>,
        extensions: Rc<dyn ShellExtensions>,
        opener: Opener,
    ) -> Rc<Self> {
        let window = ui::OnboardingWindow::new(application);
        if let Opener::Settings(parent) = opener {
            window.set_transient_for(Some(parent));
            window.set_modal(true);
        }
        let welcome = ui::OnboardingWelcome::new();
        let components_page = ui::OnboardingComponents::new();
        let shortcut_page = ui::OnboardingShortcut::new();

        let navigation = window.navigation();
        for (step, content) in [
            (Step::Welcome, welcome.upcast_ref::<gtk::Widget>()),
            (Step::Components, components_page.upcast_ref()),
            (Step::Shortcut, shortcut_page.upcast_ref()),
        ] {
            let toolbar = adw::ToolbarView::new();
            // Each page already heads itself with its title.
            toolbar.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
            toolbar.set_content(Some(content));
            navigation.add(&adw::NavigationPage::with_tag(
                &toolbar,
                &step_title(step),
                step_name(step),
            ));
        }

        let shortcut = crate::shortcut_ui::ShortcutControl::attach(
            shortcut_page.shortcut_box(),
            shortcut_page.shortcut_button(),
            window.overlay(),
            crate::shortcut_ui::Surface::Onboarding,
            shortcut_page.in_place(),
            Box::new({
                let description = shortcut_page.description();
                move |state, _| {
                    description.set_label(&crate::shortcut_ui::onboarding_description(state))
                }
            }),
        );
        let ui = Rc::new(Self {
            window: window.clone(),
            keeps_settings: matches!(opener, Opener::Settings(_)),
            components_page: components_page.clone(),
            shortcut_page: shortcut_page.clone(),
            shortcut,
            repository,
            configurator,
            extensions,
            step: Cell::new(Step::first()),
            components: RefCell::new(initial),
            offer: Cell::new(model_offer(&Machine {
                nvidia_gpu: crate::machine::has_nvidia_gpu(),
                ..Machine::default()
            })),
            problem: RefCell::default(),
            busy: Cell::new(false),
            running: Cell::new(false),
            run_step: Cell::new(None),
            epoch: Cell::new(0),
            installs: RefCell::default(),
            install_cancellation: CancellationToken::new(),
            follow_interval: Cell::new(FOLLOW_INTERVAL),
            stage: RefCell::default(),
            setup_cancellation: RefCell::default(),
            setup_slow: Cell::new(false),
            setup_failed: Cell::new(false),
            spinner_timer: RefCell::default(),
            logged: RefCell::default(),
            assessing: Cell::new(false),
            poll_interval: Cell::new(POLL_INTERVAL),
            poll: RefCell::new(None),
            beat_length: Cell::new(BEAT),
            beat: RefCell::new(None),
        });

        ui.shortcut.connect_changed(Box::new({
            let ui = Rc::downgrade(&ui);
            move || {
                if let Some(ui) = ui.upgrade() {
                    if ui.step.get() == Step::Shortcut {
                        ui.render();
                    }
                }
            }
        }));
        components_page.install_button().connect_clicked({
            let ui = Rc::downgrade(&ui);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.install_all();
                }
            }
        });

        window.forward_button().connect_clicked({
            let ui = Rc::downgrade(&ui);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.advance();
                }
            }
        });
        // The visible page is the step: going back is the navigation view's
        // own, from the header bar, Escape or Alt+Left.
        navigation.connect_visible_page_notify({
            let ui = Rc::downgrade(&ui);
            move |navigation| {
                let step = navigation
                    .visible_page()
                    .and_then(|page| page.tag())
                    .and_then(|tag| step_named(&tag));
                if let (Some(ui), Some(step)) = (ui.upgrade(), step) {
                    ui.step.set(step);
                    ui.render();
                }
            }
        });
        // What changes outside the wizard (the Extensions app, a terminal)
        // shows once the window regains focus.
        window.connect_is_active_notify({
            let ui = Rc::downgrade(&ui);
            move |window| {
                if let Some(ui) = ui.upgrade() {
                    if window.is_active()
                        && ui.step.get() == Step::Components
                        && !ui.busy.get()
                        && !ui.running.get()
                    {
                        ui.refresh_assessment();
                    }
                }
            }
        });
        // The application owns the window, so it outlives this call; without a
        // strong reference living alongside it every button would upgrade a
        // dead weak reference and do nothing. The reference is dropped when
        // the window closes, which breaks the cycle it forms.
        // Closing also stops a setup still waiting on snapd, so nothing is
        // connected or restarted behind a closed wizard.
        window.connect_close_request({
            let held = RefCell::new(Some(ui.clone()));
            move |_| {
                if let Some(ui) = held.borrow_mut().take() {
                    ui.install_cancellation.cancel();
                    if let Some(cancellation) = ui.setup_cancellation.take() {
                        ui.log("setup: cancelled, the wizard closed");
                        cancellation.cancel();
                    }
                }
                glib::Propagation::Proceed
            }
        });

        crate::app::install_appearance_policy(window.upcast_ref());
        ui.render();
        window.present();
        ui
    }

    /// Re-read the machine and re-render. Costs one `snap list` and one
    /// discovery, and runs when the window regains focus on the component
    /// step and while that step polls.
    fn refresh_assessment(self: &Rc<Self>) {
        if self.assessing.replace(true) {
            return;
        }
        let epoch = self.epoch.get();
        let ui = Rc::downgrade(self);
        let repository = self.repository.clone();
        let configurator = self.configurator.clone();
        let extensions = self.extensions.clone();
        glib::spawn_future_local(async move {
            let reading = read_machine(
                repository.as_ref(),
                configurator.as_ref(),
                extensions.as_ref(),
            )
            .await;
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.assessing.set(false);
            if ui.epoch.get() != epoch {
                ui.refresh_assessment();
                return;
            }
            let before = ui.shown();
            ui.take_reading(reading);
            if !ui.running.get() {
                ui.follow_installs_elsewhere().await;
            }
            // The user installed the last piece while watching: finish for
            // them, as Next would.
            let finish = ui.step.get() == Step::Components
                && !ui.busy.get()
                && !ui.running.get()
                && completes(&before, &ui.shown());
            if finish {
                ui.finish_setup(Step::Shortcut, true);
            } else {
                ui.render();
            }
        });
    }

    /// Take what a read found as the machine, settling the installs it
    /// confirms.
    fn take_reading(&self, (components, offer, problem): Reading) {
        self.installs
            .borrow_mut()
            .retain(|_, install| *install != Install::Confirming);
        self.offer.set(offer);
        match &problem {
            Some(problem) => self.log(&format!("assessment: {problem}")),
            None => self.log(&format!("assessment: {}", describe(&components))),
        }
        self.problem.replace(problem);
        self.components.replace(components);
    }

    /// The components as the step shows them: one snapd is still
    /// installing is missing until its change is done.
    fn shown(&self) -> Vec<Component> {
        let installing: Vec<ComponentId> = self.installs.borrow().keys().copied().collect();
        while_installing(&self.components.borrow(), &installing)
    }

    /// What is installing, the button's run or a change started elsewhere,
    /// with its download's percentage once known.
    fn in_progress(&self) -> Option<(ComponentId, Option<u8>)> {
        let installs = self.installs.borrow();
        let id = self
            .run_step
            .get()
            .or_else(|| installs.keys().next().copied())?;
        let percent = match installs.get(&id) {
            Some(Install::Running(percent)) => *percent,
            _ => None,
        };
        Some((id, percent))
    }

    /// Install every missing component the wizard can, in order: the flag,
    /// the app, the model and the extension. One polkit prompt is the only
    /// question: the flag, the snaps and the model's connect go through one
    /// privileged set-up. A dismissed prompt stops the run silently, a
    /// failure with a toast whose Details open the report; either way the
    /// button offers again what is still missing. Once nothing is left it
    /// sets dictation up and moves on, as Next would.
    fn install_all(self: &Rc<Self>) {
        if self.running.get()
            || self.busy.get()
            || !self.installs.borrow().is_empty()
            || next_install(&self.shown(), &[]).is_none()
        {
            return;
        }
        self.running.set(true);
        self.log("install: installing every missing component");
        self.render();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut done = Vec::new();
            let failure = loop {
                let Some(id) = ui.upgrade().and_then(|ui| next_install(&ui.shown(), &done)) else {
                    break None;
                };
                if let Some(ui) = ui.upgrade() {
                    ui.run_step.set(Some(id));
                    ui.render();
                }
                let set_up = ui
                    .upgrade()
                    .map(|ui| set_up_steps(&ui.shown(), &done))
                    .unwrap_or_default();
                let outcome = if set_up.is_empty() {
                    extension_step(&ui)
                        .await
                        .map(|()| vec![id])
                        .map_err(|error| (id, error))
                } else {
                    set_up_step(&ui, &set_up).await.map(|()| set_up)
                };
                match outcome {
                    Ok(steps) => {
                        done.extend(steps);
                        reread(&ui).await;
                    }
                    Err((id, error)) => break Some((id, error)),
                }
            };
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.running.set(false);
            ui.run_step.set(None);
            match failure {
                None => {
                    ui.log("install: done");
                    if ui.step.get() == Step::Components && !ui.busy.get() && settled(&ui.shown()) {
                        ui.finish_setup(Step::Shortcut, true);
                        return;
                    }
                }
                Some((_, SystemConfiguratorError::Cancelled)) => {}
                Some((id, error)) => ui.announce_failure(install_failed(id), &error),
            }
            ui.render();
        });
    }

    /// Follow an install snapd is already running for a missing row, one
    /// started in a terminal or by a wizard since closed, rather than offer
    /// to start it again. Costs one read of snapd's changes, and only while
    /// such a row is missing.
    async fn follow_installs_elsewhere(self: &Rc<Self>) {
        let offer = self.offer.get();
        let missing: Vec<(ComponentId, &'static str, u64)> = self
            .shown()
            .iter()
            .filter(|component| component.state == ComponentState::Missing)
            .filter(|component| !self.installs.borrow().contains_key(&component.id))
            .filter_map(|component| {
                installs(component.id, &offer).map(|(snap, bytes)| (component.id, snap, bytes))
            })
            .collect();
        if missing.is_empty() || !flag_enabled(&self.components.borrow()) {
            return;
        }
        let Ok(changes) = self
            .configurator
            .changes_in_progress(CancellationToken::new())
            .await
        else {
            return;
        };
        for (id, snap, expected) in missing {
            if self.installs.borrow().contains_key(&id) {
                continue;
            }
            if let Some(change) = pending_install(&changes, snap) {
                self.follow_elsewhere(id, snap, change.id().to_owned(), expected);
            }
        }
    }

    fn follow_elsewhere(
        self: &Rc<Self>,
        id: ComponentId,
        snap: &str,
        change_id: String,
        expected: u64,
    ) {
        self.installs
            .borrow_mut()
            .insert(id, Install::Running(None));
        self.log(&format!(
            "install: following change {change_id} installing {snap}"
        ));
        let ui = Rc::downgrade(self);
        let configurator = self.configurator.clone();
        let cancellation = self.install_cancellation.clone();
        let interval = self.follow_interval.get();
        let snap = snap.to_owned();
        glib::spawn_future_local(async move {
            let sleep = |interval| -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
                Box::pin(glib::timeout_future(interval))
            };
            let follow = Follow {
                interval,
                sleep: &sleep,
                cancellation,
            };
            let report = progress_reporter(ui.clone(), id);
            let outcome = follow_change(
                configurator.as_ref(),
                &change_id,
                expected,
                &follow,
                &report,
            )
            .await;
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.epoch.set(ui.epoch.get() + 1);
            // Not the user's action here, so a failure only puts Install back.
            match outcome {
                Ok(()) => {
                    ui.log(&format!("install: {snap} installed"));
                    ui.installs.borrow_mut().insert(id, Install::Confirming);
                    ui.refresh_assessment();
                }
                Err(failure) => {
                    ui.log(&format!("install: change {change_id} ended: {failure:?}"));
                    ui.installs.borrow_mut().remove(&id);
                }
            }
            ui.render();
        });
    }

    fn announce_failure(&self, heading: String, error: &SystemConfiguratorError) {
        self.toast_report(
            heading,
            crate::backend_ui::system_failure_summary(error),
            crate::backend_ui::system_error_details(error),
        );
    }

    /// A toast whose Details open the report.
    fn toast_report(&self, heading: String, summary: String, details: String) {
        let toast = adw::Toast::builder()
            .title(crate::markup::escape_markup(&heading))
            .button_label(gettextrs::gettext("Details"))
            .build();
        toast.connect_button_clicked({
            let window = self.window.clone();
            move |_| {
                ui::OperationErrorDialog::new(&heading, &summary, &details).present(Some(&window));
            }
        });
        self.window.overlay().add_toast(toast);
    }

    fn advance(self: &Rc<Self>) {
        // The gate lives here, not on the button: a step can also be advanced
        // by activating the button from the keyboard or a screen reader, and
        // an insensitive widget still emits `clicked` when told to.
        let step = self.step.get();
        let held = self.busy.get() || self.running.get();
        if (step == Step::Components && held) || !can_advance(step, &self.shown()) {
            return;
        }
        // Set up already; only the pause before moving on is left.
        if let Some(beat) = self.beat.take() {
            beat.remove();
            self.move_on(Step::Shortcut);
            return;
        }
        match self.step.get().next() {
            Some(step) if self.step.get() == Step::Components => self.finish_setup(step, false),
            Some(step) => self.window.navigation().push_by_tag(step_name(step)),
            None => self.finish(),
        }
    }

    /// Connect the backend and restart the daemon against it, so the next
    /// step finds dictation running. A store install usually auto-connects
    /// the backend; when it did not, snapd asks polkit once. With `pause`, the
    /// step first shows that everything is installed for a beat.
    fn finish_setup(self: &Rc<Self>, next: Step, pause: bool) {
        if self.busy.replace(true) {
            return;
        }
        let cancellation = CancellationToken::new();
        self.setup_cancellation.replace(Some(cancellation.clone()));
        self.setup_slow.set(false);
        self.setup_failed.set(false);
        self.spinner_timer
            .replace(Some(glib::timeout_add_local_once(SPINNER_DELAY, {
                let ui = Rc::downgrade(self);
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.spinner_timer.take();
                        ui.setup_slow.set(true);
                        ui.render();
                    }
                }
            })));
        self.render();
        let ui = Rc::downgrade(self);
        let repository = self.repository.clone();
        let configurator = self.configurator.clone();
        let interval = self.poll_interval.get();
        let previous_owner = self.shortcut.owner().flatten();
        glib::spawn_future_local(async move {
            let sleep = |interval| -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
                Box::pin(glib::timeout_future(interval))
            };
            let wait = SnapdWait {
                interval,
                timeout: SNAPD_TIMEOUT,
                sleep: &sleep,
                cancellation,
            };
            let report = {
                let ui = ui.clone();
                move |stage: SetupStage| {
                    if let Some(ui) = ui.upgrade() {
                        ui.log(&format!("setup: {}", stage_log(&stage)));
                        ui.stage.replace(Some(stage));
                        ui.render();
                    }
                }
            };
            let outcome = ensure_backend_active(
                repository.as_ref(),
                configurator.as_ref(),
                RECOMMENDED_BACKEND_SNAP,
                &wait,
                &report,
            )
            .await;
            if outcome == Ok(Settled::Restarted) {
                wait_for_daemon(&ui, previous_owner).await;
            }
            let Some(ui) = ui.upgrade() else {
                return;
            };
            match &outcome {
                Ok(_) => ui.log("setup: done"),
                Err(SetupError::Cancelled) => ui.log("setup: cancelled"),
                Err(error) => ui.log(&format!("setup: failed: {error}")),
            }
            ui.setup_cancellation.take();
            if let Some(timer) = ui.spinner_timer.take() {
                timer.remove();
            }
            ui.setup_slow.set(false);
            ui.stage.take();
            ui.busy.set(false);
            ui.setup_failed.set(outcome.is_err());
            ui.render();
            // Back during the first second leaves the setup to finish there.
            let here = ui.step.get() == Step::Components;
            match outcome {
                Ok(_) if !here => {}
                Ok(_) if pause => ui.pause_before(next),
                Ok(_) => ui.move_on(next),
                // Dismissing the prompt was the user's answer; Next asks again.
                Err(SetupError::Cancelled) => {}
                Err(SetupError::Failed(message)) => ui.announce_setup_failure(message),
                Err(SetupError::Busy(change)) => ui.toast_report(
                    gettextrs::gettext("Could not set up Dictation"),
                    gettextrs::gettext(
                        "The system is busy installing software. Try again in a moment.",
                    ),
                    change,
                ),
                Err(SetupError::Step(error)) => {
                    ui.announce_failure(gettextrs::gettext("Could not set up Dictation"), &error)
                }
            }
        });
    }

    /// Move on to `next` once the status has been readable for a beat.
    fn pause_before(self: &Rc<Self>, next: Step) {
        let ui = Rc::downgrade(self);
        let beat = glib::timeout_add_local_once(self.beat_length.get(), move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.beat.take();
            ui.move_on(next);
        });
        self.beat.replace(Some(beat));
    }

    /// Show the shortcut step, setting the default key as it arrives.
    fn move_on(&self, next: Step) {
        self.window.navigation().push_by_tag(step_name(next));
        self.shortcut.install_default();
    }

    /// The probe polls and pauses shorter than a person needs.
    pub fn set_poll_interval(&self, interval: Duration) {
        self.poll_interval.set(interval);
        self.follow_interval.set(interval);
    }

    pub fn set_beat(&self, beat: Duration) {
        self.beat_length.set(beat);
    }

    /// The widgets the headless probe drives the wizard through. It holds
    /// these and drops the controller, the way the application does.
    pub fn window(&self) -> ui::OnboardingWindow {
        self.window.clone()
    }

    pub fn shortcut_button(&self) -> gtk::Button {
        self.shortcut_page.shortcut_button()
    }

    pub fn shortcut(&self) -> Rc<crate::shortcut_ui::ShortcutControl> {
        self.shortcut.clone()
    }

    /// Done closes only the wizard when it was opened from the settings
    /// window, which stays and re-reads the machine; on first run it closes
    /// Myna Settings. Closing, not quitting, so each window's close handler
    /// still stops what it runs.
    fn finish(&self) {
        if self.keeps_settings {
            self.window.close();
            return;
        }
        let windows = self
            .window
            .application()
            .map(|application| application.windows())
            .unwrap_or_else(|| vec![self.window.clone().upcast()]);
        self.window.close();
        for window in windows {
            window.close();
        }
    }

    fn render(self: &Rc<Self>) {
        let step = self.step.get();
        let components = self.shown();

        self.window.set_title(Some(&step_title(step)));
        // Setting up restarts the daemon; leaving mid-way would strand it.
        // Before the spinner shows the arrow stays: hiding it for a
        // fraction of a second flashed it.
        let setting_up = step == Step::Components && self.busy.get() && self.setup_slow.get();
        if let Some(page) = self.window.navigation().visible_page() {
            page.set_can_pop(!setting_up);
        }

        self.render_components(&components, setting_up);
        self.shortcut_page
            .relogin_note()
            .set_visible(relogin_pending(&components));
        let forward = self.window.forward_button();
        if step.next().is_some() {
            forward.set_label(&gettextrs::gettext("Next"));
            forward.update_property(&[gtk::accessible::Property::Description(
                &gettextrs::gettext("Continue to the next onboarding step."),
            )]);
        } else {
            forward.set_label(&gettextrs::gettext("Done"));
            forward.update_property(&[gtk::accessible::Property::Description(
                &gettextrs::gettext("Close Myna Settings."),
            )]);
        }
        let held = match step {
            Step::Components => self.busy.get() || self.running.get(),
            // Done under a capture in place would drop the key half chosen.
            Step::Shortcut => self.shortcut.capturing(),
            Step::Welcome => false,
        };
        set_class(
            &forward,
            "suggested-action",
            !held && forward_leads(step, &components, self.shortcut.needs_key()),
        );
        forward.set_sensitive(!held && can_advance(step, &components));
        self.watch(!self.busy.get() && !self.running.get() && polls(step, &components));
        // Going back during the pause stays back.
        if step != Step::Components {
            if let Some(beat) = self.beat.take() {
                beat.remove();
            }
        }
    }

    /// The one button, as the last assessment and what is installing make
    /// it, and the line under it: what installing downloads, what is under
    /// way, or how setting up went.
    fn render_components(&self, components: &[Component], setting_up: bool) {
        let page = &self.components_page;
        let progress = self.in_progress();
        let view = install_view(components, progress.map(|(id, _)| id));
        let offer = view == InstallView::Offer;
        page.show_install_label(match view {
            InstallView::Offer => "offer",
            InstallView::Installing(_) => "installing",
            InstallView::Installed => "installed",
        });
        let button = page.install_button();
        button.set_sensitive(offer && !self.busy.get());
        // Next leads once nothing required is missing; insensitive, the
        // accent would only read as a faded call to act.
        set_class(
            &button,
            "suggested-action",
            offer && needs_onboarding(components),
        );
        let stage = self.stage.borrow();
        let failed = self.setup_failed.get() && !self.busy.get() && !needs_onboarding(components);
        let text = match (&*stage, progress) {
            (Some(stage), _) if setting_up => Some(stage_text(stage)),
            (_, Some((id, percent))) => Some(step_text(id, percent)),
            _ => None,
        };
        let size = if offer {
            match remaining_download(components, &self.offer.get()) {
                DownloadSize::Exact(0) => String::new(),
                DownloadSize::Exact(bytes) => download_size(bytes),
                DownloadSize::UpTo(bytes) => {
                    // TRANSLATORS: {size} is a download size, such as "4.2 GB".
                    let frame = gettextrs::gettext("Up to {size}");
                    frame.replace("{size}", &download_size(bytes))
                }
            }
        } else {
            String::new()
        };
        // The button carries the size too, so it is spoken with the button.
        page.describe_install(&size);
        let note = if failed {
            gettextrs::gettext("Dictation is not set up yet. Select Next to try again.")
        } else if needs_onboarding(components) && self.problem.borrow().is_some() {
            gettextrs::gettext("Setup status unavailable. The log has the details.")
        } else {
            size
        };
        page.show_status(match &text {
            Some(text) => ui::ComponentsStatus::Busy(text),
            None if failed => ui::ComponentsStatus::Warning(&note),
            None if note.is_empty() => ui::ComponentsStatus::Hidden,
            None => ui::ComponentsStatus::Note(&note),
        });
    }

    /// Start or stop the component step's poll.
    fn watch(self: &Rc<Self>, wanted: bool) {
        let mut poll = self.poll.borrow_mut();
        if !wanted {
            if let Some(source) = poll.take() {
                source.remove();
            }
            return;
        }
        if poll.is_none() {
            let ui = Rc::downgrade(self);
            *poll = Some(glib::timeout_add_local(
                self.poll_interval.get(),
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.refresh_assessment();
                    }
                    glib::ControlFlow::Continue
                },
            ));
        }
    }

    /// Say what the wizard found or did, once per change, to the journal
    /// when launched from the desktop and to stderr from a terminal.
    fn log(&self, line: &str) {
        if *self.logged.borrow() != line {
            glib::g_message!(crate::LOG_DOMAIN, "onboarding {}", line);
            self.logged.replace(line.to_owned());
        }
    }

    /// A toast rather than a dialog: setting up may have started by itself,
    /// and the step stays usable, Next retrying.
    fn announce_setup_failure(&self, message: String) {
        let heading = gettextrs::gettext("Could not set up Dictation");
        self.toast_report(heading.clone(), heading, message);
    }
}

impl Drop for OnboardingUi {
    fn drop(&mut self) {
        for source in [
            self.poll.take(),
            self.beat.take(),
            self.spinner_timer.take(),
        ]
        .into_iter()
        .flatten()
        {
            source.remove();
        }
    }
}

/// Wait for the restarted daemon, a new owner of its name, so the shortcut
/// step arrives showing the key rather than "not running". A daemon that does
/// not start in time is left for that step to show.
async fn wait_for_daemon(ui: &std::rc::Weak<OnboardingUi>, previous: Option<String>) {
    let mut waited = Duration::ZERO;
    loop {
        let Some(strong) = ui.upgrade() else {
            return;
        };
        // Closing the wizard takes the cancellation.
        if strong.setup_cancellation.borrow().is_none() {
            return;
        }
        match strong.shortcut.owner() {
            None => return,
            Some(Some(owner)) if Some(&owner) != previous.as_ref() => return,
            Some(_) => {}
        }
        if waited >= DAEMON_START_TIMEOUT {
            strong.log("setup: the daemon did not claim its name in time");
            return;
        }
        drop(strong);
        let interval = Duration::from_millis(100);
        glib::timeout_future(interval).await;
        waited += interval;
    }
}

/// What a row's install reports hands to the row.
fn progress_reporter(ui: std::rc::Weak<OnboardingUi>, id: ComponentId) -> impl Fn(Option<u8>) {
    move |percent| {
        if let Some(ui) = ui.upgrade() {
            ui.installs
                .borrow_mut()
                .insert(id, Install::Running(percent));
            ui.render();
        }
    }
}

/// A step of the button's run as the line under it says it, beside the
/// spinner.
fn step_text(id: ComponentId, percent: Option<u8>) -> String {
    let step = match id {
        ComponentId::UserDaemons => gettextrs::gettext("Enabling user daemons support"),
        ComponentId::Myna => gettextrs::gettext("Installing Dictation app"),
        ComponentId::Model => gettextrs::gettext("Installing speech-to-text model"),
        ComponentId::ShellExtension => gettextrs::gettext("Enabling shell extension"),
    };
    match percent {
        Some(percent) => {
            // TRANSLATORS: {step} is what is installing, such as "Installing speech-to-text model", and {percent} how much of its download has arrived.
            let frame = gettextrs::gettext("{step} ({percent}%)");
            frame
                .replace("{step}", &step)
                .replace("{percent}", &percent.to_string())
        }
        None => step,
    }
}

/// The toast's heading when a step of the run fails.
fn install_failed(id: ComponentId) -> String {
    match id {
        ComponentId::UserDaemons => gettextrs::gettext("Could not let Myna run in the background"),
        ComponentId::Myna => gettextrs::gettext("Installing the Dictation app failed"),
        ComponentId::Model => gettextrs::gettext("Installing the speech-to-text model failed"),
        ComponentId::ShellExtension => gettextrs::gettext("Enabling the shell extension failed"),
    }
}

/// The run's last step: the extension, asked of the user's own
/// gnome-shell, which needs no authorization.
async fn extension_step(ui: &std::rc::Weak<OnboardingUi>) -> Result<(), SystemConfiguratorError> {
    let Some(strong) = ui.upgrade() else {
        return Err(SystemConfiguratorError::Cancelled);
    };
    strong.epoch.set(strong.epoch.get() + 1);
    strong.log("extension: enabling");
    let extensions = strong.extensions.clone();
    drop(strong);
    let outcome = extensions.enable_extension(SHELL_EXTENSION_UUID).await;
    if let Some(ui) = ui.upgrade() {
        ui.epoch.set(ui.epoch.get() + 1);
        match &outcome {
            Ok(()) => ui.log("extension: enabled"),
            Err(error) => ui.log(&format!("extension: failed: {error}")),
        }
    }
    outcome
}

/// `steps` as one privileged set-up, one prompt: the flag, the snaps and the
/// model's connect (`onboarding::set_up_plan`). snapd's changes are read
/// meanwhile, so the line under the button names the snap installing and its
/// download. The executor is root and outlives a closed wizard, which only
/// stops following.
async fn set_up_step(
    ui: &std::rc::Weak<OnboardingUi>,
    steps: &[ComponentId],
) -> Result<(), (ComponentId, SystemConfiguratorError)> {
    let cancelled = (ComponentId::UserDaemons, SystemConfiguratorError::Cancelled);
    let Some(strong) = ui.upgrade() else {
        return Err(cancelled);
    };
    strong.epoch.set(strong.epoch.get() + 1);
    let offer = strong.offer.get();
    let snaps: Vec<(ComponentId, &'static str, u64)> = steps
        .iter()
        .filter_map(|id| installs(*id, &offer).map(|(snap, bytes)| (*id, snap, bytes)))
        .collect();
    let plan = set_up_plan(steps, &offer);
    strong.log(&format!("set-up: {plan:?} under one prompt"));
    let configurator = strong.configurator.clone();
    let cancellation = strong.install_cancellation.clone();
    let interval = strong.follow_interval.get();
    drop(strong);

    let finished = Rc::new(RefCell::new(None));
    glib::spawn_future_local({
        let configurator = configurator.clone();
        let finished = finished.clone();
        async move {
            let outcome = configurator.set_up(&plan, CancellationToken::new()).await;
            finished.replace(Some(outcome));
        }
    });
    let mut highest = BTreeMap::new();
    let outcome = loop {
        if let Some(outcome) = finished.take() {
            break outcome;
        }
        if cancellation.is_cancelled() {
            return Err(cancelled);
        }
        if let Ok(changes) = configurator.changes_in_progress(cancellation.clone()).await {
            let Some(ui) = ui.upgrade() else {
                return Err(cancelled);
            };
            for (id, snap, expected) in &snaps {
                let Some(change) = pending_install(&changes, snap) else {
                    continue;
                };
                let highest = highest.entry(*id).or_insert(0);
                let percent = change
                    .download_percent(*expected)
                    .map(|percent| percent.max(*highest));
                *highest = percent.unwrap_or(*highest);
                ui.run_step.set(Some(*id));
                ui.installs
                    .borrow_mut()
                    .insert(*id, Install::Running(percent));
                ui.render();
            }
        }
        glib::timeout_future(interval).await;
    };

    let Some(ui) = ui.upgrade() else {
        return outcome.map_err(|error| (ComponentId::UserDaemons, error));
    };
    ui.epoch.set(ui.epoch.get() + 1);
    let mut installs = ui.installs.borrow_mut();
    for (id, _, _) in &snaps {
        match outcome {
            Ok(()) => installs.insert(*id, Install::Confirming),
            Err(_) => installs.remove(id),
        };
    }
    drop(installs);
    outcome.map_err(|error| {
        let failed = failed_set_up_step(error.step());
        match &error {
            SystemConfiguratorError::Cancelled => ui.log("set-up: the prompt was dismissed"),
            error => ui.log(&format!("set-up: {failed:?} failed: {error}")),
        }
        (failed, error)
    })?;
    ui.log("set-up: done");
    Ok(())
}

/// Re-read the machine between the run's steps.
async fn reread(ui: &std::rc::Weak<OnboardingUi>) {
    let Some(strong) = ui.upgrade() else {
        return;
    };
    let (repository, configurator, extensions) = (
        strong.repository.clone(),
        strong.configurator.clone(),
        strong.extensions.clone(),
    );
    drop(strong);
    let reading = read_machine(
        repository.as_ref(),
        configurator.as_ref(),
        extensions.as_ref(),
    )
    .await;
    if let Some(ui) = ui.upgrade() {
        ui.take_reading(reading);
    }
}

/// One assessment of what dictation is missing on this machine: one `snap
/// list`, one discovery, snapd's flags over its socket and one call to
/// gnome-shell. A surface that cannot be read counts as nothing found, which
/// opens the wizard: the flow then shows what it could not verify rather than
/// a settings window with no backends and no explanation.
pub async fn assess_machine(
    repository: &dyn BackendRepository,
    configurator: &dyn SystemConfigurator,
    extensions: &dyn ShellExtensions,
) -> Vec<Component> {
    let (components, _, problem) = read_machine(repository, configurator, extensions).await;
    let found = problem.unwrap_or_else(|| describe(&components));
    glib::g_message!(crate::LOG_DOMAIN, "onboarding assessment: {found}");
    components
}

/// What one read finds: the components, the model the wizard would install,
/// and what it could not read.
type Reading = (Vec<Component>, ModelOffer, Option<String>);

/// [`assess_machine`], the model it would install, and what it could not
/// read.
async fn read_machine(
    repository: &dyn BackendRepository,
    configurator: &dyn SystemConfigurator,
    extensions: &dyn ShellExtensions,
) -> Reading {
    let cancellation = CancellationToken::new();
    let mut problems = Vec::new();
    let user_daemons = configurator
        .user_daemons_enabled(cancellation.clone())
        .await
        .unwrap_or_else(|error| {
            problems.push(error);
            false
        });
    let installed = repository
        .installed_snaps(cancellation.clone())
        .await
        .unwrap_or_else(|error| {
            problems.push(reason(&error));
            Vec::new()
        });
    let backends = repository
        .discover(cancellation)
        .await
        .map(|snapshot| snapshot.backends().len())
        .unwrap_or_else(|error| {
            problems.push(reason(&error));
            0
        });
    problems.dedup();
    // snapd's own words, for the log; the step shows a plain sentence.
    let problem = (!problems.is_empty()).then(|| problems.join("; "));
    let machine = Machine {
        user_daemons,
        extension: extensions.extension_state(SHELL_EXTENSION_UUID).await,
        nvidia_gpu: crate::machine::has_nvidia_gpu(),
        ..Machine::new(&installed, backends)
    };
    (assess(machine), model_offer(&machine), problem)
}

/// What snap said, which names the cause, over how it exited.
fn reason(error: &BackendSurfaceError) -> String {
    match error.stderr().trim() {
        "" => error.message().to_owned(),
        stderr => stderr.to_owned(),
    }
}

fn describe(components: &[Component]) -> String {
    components
        .iter()
        .map(|component| {
            let state = match component.state {
                ComponentState::Satisfied => "found".to_owned(),
                ComponentState::AfterRelogin => "found, after a re-login".to_owned(),
                ComponentState::Missing => "missing".to_owned(),
                ComponentState::Unavailable(why) => format!("unavailable ({why:?})"),
            };
            format!("{:?} {state}", component.id)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A download's size as the rows show it: in whole megabytes from 100 MB to
/// 1 GB, where a decimal only adds noise.
pub(crate) fn download_size(bytes: u64) -> String {
    if !(100_000_000..999_500_000).contains(&bytes) {
        return glib::format_size(bytes).to_string();
    }
    // TRANSLATORS: a download size in megabytes, such as "776 MB"; the space is a no-break space.
    gettextrs::gettext("{megabytes} MB")
        .replace("{megabytes}", &((bytes + 500_000) / 1_000_000).to_string())
}

/// A stage as the line under the button says it, beside the spinner, in the
/// install steps' words and, like them, without an ellipsis.
fn stage_text(stage: &SetupStage) -> String {
    match stage {
        SetupStage::Checking => gettextrs::gettext("Checking the installation"),
        SetupStage::Waiting(progress @ ApplyProgress::Download { .. }) => {
            crate::backend_ui::apply_progress_text(progress)
        }
        SetupStage::Waiting(ApplyProgress::Change { .. }) => {
            gettextrs::gettext("Waiting for other software changes to finish")
        }
        SetupStage::Connecting(_) => gettextrs::gettext("Setting up speech-to-text model"),
        SetupStage::Restarting => gettextrs::gettext("Starting dictation"),
    }
}

/// A stage as the log says it: a download once, not at every byte count.
fn stage_log(stage: &SetupStage) -> String {
    match stage {
        SetupStage::Checking => "checking the connections and snapd".to_owned(),
        SetupStage::Waiting(ApplyProgress::Download { name, total, .. }) => {
            format!(
                "waiting for snapd to download {name} ({})",
                glib::format_size(*total)
            )
        }
        SetupStage::Waiting(ApplyProgress::Change { summary }) => {
            format!("waiting for snapd: {summary}")
        }
        SetupStage::Connecting(snap) => format!("connecting myna:backend to {snap}"),
        SetupStage::Restarting => "restarting snap.myna.myna.service".to_owned(),
    }
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Welcome => "welcome",
        Step::Components => "components",
        Step::Shortcut => "shortcut",
    }
}

fn step_named(name: &str) -> Option<Step> {
    [Step::Welcome, Step::Components, Step::Shortcut]
        .into_iter()
        .find(|step| step_name(*step) == name)
}

fn step_title(step: Step) -> String {
    match step {
        Step::Welcome => gettextrs::gettext("Dictation"),
        Step::Components => gettextrs::gettext("Install components"),
        Step::Shortcut => gettextrs::gettext("How to dictate"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_on_snapd_keeps_its_english_summary_for_the_log() {
        let stage = SetupStage::Waiting(ApplyProgress::Change {
            summary: "Auto-refresh snap \"myna\"".to_owned(),
        });

        assert_eq!(
            stage_text(&stage),
            "Waiting for other software changes to finish"
        );
        assert_eq!(
            stage_log(&stage),
            "waiting for snapd: Auto-refresh snap \"myna\""
        );
    }

    #[test]
    fn a_connect_names_the_model_not_its_engine() {
        let stage = SetupStage::Connecting("myna-parakeet".to_owned());

        assert_eq!(stage_text(&stage), "Setting up speech-to-text model");
        assert_eq!(
            stage_log(&stage),
            "connecting myna:backend to myna-parakeet"
        );
    }

    #[test]
    fn a_download_over_100_mb_is_sized_in_whole_megabytes() {
        assert_eq!(download_size(10_133_504), glib::format_size(10_133_504));
        assert_eq!(download_size(775_593_984), "776\u{a0}MB");
        assert_eq!(download_size(100_000_000), "100\u{a0}MB");
        assert_eq!(download_size(99_999_999), glib::format_size(99_999_999));
        assert_eq!(download_size(999_499_999), "999\u{a0}MB");
        assert_eq!(download_size(999_500_000), glib::format_size(999_500_000));
        assert_eq!(
            download_size(4_172_693_504),
            glib::format_size(4_172_693_504)
        );
    }
}
