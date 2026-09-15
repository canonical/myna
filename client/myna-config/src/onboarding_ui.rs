//! GTK/libadwaita wiring for the onboarding wizard.
//!
//! Thin, like [`crate::backend_ui`]: [`crate::onboarding`] decides what is
//! missing, who can fix it, and when the flow may advance; this module renders
//! that, runs the one install the application is allowed to run, and hands
//! control back to the settings window when the user is done.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::active_backend::{execute_switch, SwitchOutcome, SwitchPlan};
use crate::adapters::snap_backend::SnapBackendRepository;
use crate::adapters::snapd_client::{InstallProgress, ProgressSink, UnixSocketSnapdClient};
use crate::adapters::system_configurator::PkexecSystemConfigurator;
use crate::command::{CancellationToken, GioCommandRunner};
use crate::domain::ActiveBackendState;
use crate::onboarding::{
    assess, can_advance, outstanding, Component, ComponentId, InstallTarget, Machine, Remedy, Step,
    MYNA_SNAP, RECOMMENDED_BACKEND_SNAP, RECOMMENDED_MODEL_MEGABYTES, SHELL_EXTENSION_UUID,
};
use crate::ports::{BackendRepository, SnapInstaller, SystemConfigurator};
use crate::ui;

pub struct OnboardingUi {
    window: ui::OnboardingWindow,
    welcome: ui::OnboardingWelcome,
    components_page: ui::OnboardingComponents,
    shortcut_page: ui::OnboardingShortcut,
    repository: Rc<dyn BackendRepository>,
    installer: Rc<dyn SnapInstaller>,
    configurator: Rc<dyn SystemConfigurator>,
    step: Cell<Step>,
    components: RefCell<Vec<Component>>,
    busy: Cell<bool>,
    finished: RefCell<Option<Box<dyn Fn()>>>,
}

impl OnboardingUi {
    /// Build and present the wizard against the real snapd and snap ports.
    /// `finished` runs once, when the user completes or closes the flow.
    pub fn present(
        application: &adw::Application,
        initial: Vec<Component>,
        finished: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let runner = Arc::new(GioCommandRunner);
        Self::present_with_ports(
            application,
            initial,
            Rc::new(SnapBackendRepository::new(runner.clone())),
            Rc::new(UnixSocketSnapdClient::new()),
            Rc::new(PkexecSystemConfigurator::new(runner)),
            finished,
        )
    }

    pub fn present_with_ports(
        application: &adw::Application,
        initial: Vec<Component>,
        repository: Rc<dyn BackendRepository>,
        installer: Rc<dyn SnapInstaller>,
        configurator: Rc<dyn SystemConfigurator>,
        finished: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let window = ui::OnboardingWindow::new(application);
        let welcome = ui::OnboardingWelcome::new();
        let components_page = ui::OnboardingComponents::new();
        let shortcut_page = ui::OnboardingShortcut::new();

        let stack = window.stack();
        stack.add_named(&welcome, Some(step_name(Step::Welcome)));
        stack.add_named(&components_page, Some(step_name(Step::Components)));
        stack.add_named(&shortcut_page, Some(step_name(Step::Shortcut)));

        let ui = Rc::new(Self {
            window: window.clone(),
            welcome: welcome.clone(),
            components_page,
            shortcut_page: shortcut_page.clone(),
            repository,
            installer,
            configurator,
            step: Cell::new(Step::first()),
            components: RefCell::new(initial),
            busy: Cell::new(false),
            finished: RefCell::new(Some(finished)),
        });

        welcome.start_button().connect_clicked({
            let ui = Rc::downgrade(&ui);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.advance();
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
        window.back_button().connect_clicked({
            let ui = Rc::downgrade(&ui);
            move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.retreat();
                }
            }
        });
        crate::shortcut_ui::ShortcutControl::attach(
            shortcut_page.shortcut_box(),
            shortcut_page.shortcut_button(),
            window.overlay(),
            false,
            Box::new({
                let description = shortcut_page.description();
                move |state| {
                    description.set_label(&crate::shortcut_ui::onboarding_description(state))
                }
            }),
        );
        // The application owns the window, so it outlives this call; without a
        // strong reference living alongside it every button would upgrade a
        // dead weak reference and do nothing. The reference is dropped when
        // the window closes, which breaks the cycle it forms.
        window.connect_close_request({
            let held = RefCell::new(Some(ui.clone()));
            move |_| {
                held.borrow_mut().take();
                glib::Propagation::Proceed
            }
        });

        crate::app::install_appearance_policy(window.upcast_ref());
        ui.render();
        window.present();
        ui
    }

    /// Re-read the machine and re-render. Costs one `snap list` and one
    /// discovery, and runs only at startup and after an install.
    fn refresh_assessment(self: &Rc<Self>) {
        let ui = Rc::downgrade(self);
        let repository = self.repository.clone();
        glib::spawn_future_local(async move {
            let cancellation = CancellationToken::new();
            let installed = repository
                .installed_snaps(cancellation.clone())
                .await
                .unwrap_or_default();
            let backends = repository
                .discover(cancellation)
                .await
                .map(|snapshot| snapshot.backends().len())
                .unwrap_or_default();
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let machine = Machine::new(&installed, backends, shell_extension_installed());
            ui.components.replace(assess(machine));
            ui.render();
        });
    }

    fn advance(self: &Rc<Self>) {
        // The gate lives here, not on the button: a step can also be advanced
        // by activating the button from the keyboard or a screen reader, and
        // an insensitive widget still emits `clicked` when told to.
        if self.busy.get() || !can_advance(self.step.get(), &self.components.borrow()) {
            return;
        }
        match self.step.get().next() {
            Some(step) => {
                self.step.set(step);
                self.render();
            }
            None => {
                self.notify_finished();
                self.window.close();
            }
        }
    }

    fn retreat(self: &Rc<Self>) {
        if let Some(step) = self.step.get().previous() {
            self.step.set(step);
            self.render();
        }
    }

    /// The widgets the headless probe drives the wizard through. It holds
    /// these and drops the controller, the way the application does.
    pub fn window(&self) -> ui::OnboardingWindow {
        self.window.clone()
    }

    pub fn start_button(&self) -> gtk::Button {
        self.welcome.start_button()
    }

    pub fn shortcut_button(&self) -> gtk::Button {
        self.shortcut_page.shortcut_button()
    }

    fn notify_finished(self: &Rc<Self>) {
        if let Some(finished) = self.finished.borrow_mut().take() {
            finished();
        }
    }

    fn render(self: &Rc<Self>) {
        let step = self.step.get();
        let components = self.components.borrow().clone();

        self.window.window_title().set_title(&step_title(step));
        self.window.set_title(Some(&step_title(step)));
        self.window.stack().set_visible_child_name(step_name(step));

        let back = self.window.back_button();
        back.set_visible(step.previous().is_some());
        back.set_sensitive(!self.busy.get());

        let forward = self.window.forward_button();
        // The welcome step has its own button in the middle of the page, so
        // the action bar carries nothing there.
        forward.set_visible(step != Step::Welcome);
        forward.set_label(&if step.next().is_some() {
            gettextrs::gettext("Next")
        } else {
            gettextrs::gettext("Done")
        });
        forward.set_sensitive(!self.busy.get() && can_advance(step, &components));

        if step == Step::Components {
            self.render_components(&components);
        }
    }

    fn render_components(self: &Rc<Self>, components: &[Component]) {
        let list = self.components_page.list();
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let missing = outstanding(components);
        if missing.is_empty() {
            self.components_page
                .subtitle()
                .set_label(&gettextrs::gettext(
                    "Everything Dictation needs is installed.",
                ));
            return;
        }
        self.components_page
            .subtitle()
            .set_label(&gettextrs::gettext(
                "You need to install some components for Dictation to work.",
            ));

        for component in missing {
            list.append(&self.component_row(component));
        }
    }

    fn component_row(self: &Rc<Self>, component: Component) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(component_title(component.id))
            .subtitle(component_detail(component.id))
            .build();
        let button = gtk::Button::builder().valign(gtk::Align::Center).build();
        match component.remedy {
            Remedy::Install(target) => {
                button.set_label(&gettextrs::gettext("Install"));
                button.set_sensitive(!self.busy.get());
                button.connect_clicked({
                    let ui = Rc::downgrade(self);
                    let row = row.clone();
                    move |_| {
                        if let Some(ui) = ui.upgrade() {
                            ui.begin_install(target, row.clone());
                        }
                    }
                });
            }
            Remedy::Explain => {
                button.set_label(&gettextrs::gettext("How to install"));
                button.connect_clicked({
                    let ui = Rc::downgrade(self);
                    let id = component.id;
                    move |_| {
                        if let Some(ui) = ui.upgrade() {
                            ui.show_instructions(id);
                        }
                    }
                });
            }
        }
        row.add_suffix(&button);
        row.set_activatable_widget(Some(&button));
        row
    }

    /// Install through snapd, then make the new backend the active one, so
    /// finishing the wizard leaves dictation working rather than installed.
    fn begin_install(self: &Rc<Self>, target: InstallTarget, row: adw::ActionRow) {
        if self.busy.replace(true) {
            return;
        }
        self.render();
        row.set_subtitle(&gettextrs::gettext("Starting…"));

        let ui = Rc::downgrade(self);
        let installer = self.installer.clone();
        let repository = self.repository.clone();
        let configurator = self.configurator.clone();
        let progress: ProgressSink = Rc::new({
            let row = row.clone();
            move |progress: InstallProgress| {
                row.set_subtitle(&install_progress_text(&progress));
            }
        });
        glib::spawn_future_local(async move {
            let outcome = installer
                .install(target, Some(progress), CancellationToken::new())
                .await;
            let Some(ui) = ui.upgrade() else {
                return;
            };
            match outcome {
                Ok(()) => {
                    row.set_subtitle(&gettextrs::gettext("Connecting…"));
                    if let Err(message) =
                        activate_backend(repository.as_ref(), configurator.as_ref(), target).await
                    {
                        ui.report_failure(
                            &gettextrs::gettext("Could not enable the backend"),
                            &message,
                        );
                    }
                }
                Err(error) => {
                    ui.report_failure(
                        &gettextrs::gettext("Could not install the component"),
                        &error.to_string(),
                    );
                }
            }
            ui.busy.set(false);
            ui.refresh_assessment();
        });
    }

    fn show_instructions(self: &Rc<Self>, id: ComponentId) {
        let (heading, body, command) = instructions(id);
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_response("close", &gettextrs::gettext("Close"));
        if let Some(command) = command.clone() {
            dialog.add_response("copy", &gettextrs::gettext("Copy command"));
            dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
            dialog.connect_response(None, {
                let window = self.window.clone();
                let overlay = self.window.overlay();
                move |_, response| {
                    if response == "copy" {
                        gtk::prelude::WidgetExt::display(&window)
                            .clipboard()
                            .set_text(&command);
                        overlay.add_toast(adw::Toast::new(&gettextrs::gettext(
                            "Install command copied to clipboard",
                        )));
                    }
                }
            });
        }
        dialog.present(Some(&self.window));
    }

    fn report_failure(self: &Rc<Self>, title: &str, details: &str) {
        let dialog = ui::OperationErrorDialog::new(title, title, details);
        dialog.present(Some(&self.window));
    }
}

/// Connect the freshly installed backend and restart the daemon against it.
/// A no-op when the backend is already the active one.
async fn activate_backend(
    repository: &dyn BackendRepository,
    configurator: &dyn SystemConfigurator,
    target: InstallTarget,
) -> Result<(), String> {
    let snapshot = repository
        .refresh(CancellationToken::new())
        .await
        .map_err(|error| error.message().to_owned())?;
    let Some(identity) = snapshot
        .backends()
        .iter()
        .find(|backend| backend.snap_name() == target.snap())
        .cloned()
    else {
        return Err(gettextrs::gettext(
            "The installed backend did not appear in snap connections.",
        ));
    };
    if matches!(snapshot.active_state(), ActiveBackendState::Connected(ref active) if active == &identity)
    {
        return Ok(());
    }
    let plan = SwitchPlan::new(&snapshot, identity)
        .map_err(|_| gettextrs::gettext("The installed backend is not available to switch to."))?;
    match execute_switch(
        &plan,
        true,
        configurator,
        repository,
        CancellationToken::new(),
    )
    .await
    {
        SwitchOutcome::Applied { .. } | SwitchOutcome::Noop { .. } => Ok(()),
        SwitchOutcome::Failed { error, .. } => Err(error.to_string()),
        SwitchOutcome::FinalDiscoveryFailed { error, .. } => Err(error.message().to_owned()),
        SwitchOutcome::Cancelled { .. } => Err(gettextrs::gettext("The change was cancelled.")),
        SwitchOutcome::Disagreed { .. } | SwitchOutcome::StaleDiscovery { .. } => Err(
            gettextrs::gettext("The backend changed while it was being enabled."),
        ),
    }
}

/// Whether the HUD's GNOME Shell extension is installed for this user or
/// system-wide.
pub fn shell_extension_installed() -> bool {
    let data_home = gio::glib::user_data_dir();
    let system: Vec<std::path::PathBuf> = gio::glib::system_data_dirs();
    crate::onboarding::shell_extension_directories(&data_home, &system)
        .iter()
        .any(|directory| directory.is_dir())
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Welcome => "welcome",
        Step::Components => "components",
        Step::Shortcut => "shortcut",
    }
}

fn step_title(step: Step) -> String {
    match step {
        Step::Welcome => gettextrs::gettext("Dictation"),
        Step::Components => gettextrs::gettext("Install components"),
        Step::Shortcut => gettextrs::gettext("How to dictate"),
    }
}

fn component_title(id: ComponentId) -> String {
    match id {
        ComponentId::Myna => gettextrs::gettext("Myna"),
        ComponentId::Model => gettextrs::gettext("Recommended speech-to-text model"),
        ComponentId::ShellExtension => gettextrs::gettext("Shell extension"),
    }
}

fn component_detail(id: ComponentId) -> String {
    match id {
        ComponentId::Myna => gettextrs::gettext("The dictation client itself"),
        ComponentId::Model => {
            // Translators: the model name, then its installed size.
            gettextrs::gettext("Parakeet · {size} MB")
                .replace("{size}", &RECOMMENDED_MODEL_MEGABYTES.to_string())
        }
        ComponentId::ShellExtension => {
            gettextrs::gettext("Needed to show dictation status in the desktop")
        }
    }
}

/// Heading, body and copyable command for a component the user has to install
/// themselves.
fn instructions(id: ComponentId) -> (String, String, Option<String>) {
    match id {
        ComponentId::Myna => (
            gettextrs::gettext("Install Myna"),
            gettextrs::gettext(
                "Myna runs as a user service, which snapd only installs once user daemons are enabled. Run both commands in a terminal.",
            ),
            Some(format!(
                "sudo snap set system experimental.user-daemons=true\nsudo snap install {MYNA_SNAP}"
            )),
        ),
        ComponentId::Model => (
            gettextrs::gettext("Install the speech-to-text model"),
            gettextrs::gettext("Run this command in a terminal."),
            Some(format!("sudo snap install {RECOMMENDED_BACKEND_SNAP}")),
        ),
        ComponentId::ShellExtension => (
            gettextrs::gettext("Install the shell extension"),
            gettextrs::gettext(
                "The extension is not published in a store yet. Copy it into the extensions directory and enable it, then log out and back in.",
            ),
            Some(format!("gnome-extensions enable {SHELL_EXTENSION_UUID}")),
        ),
    }
}

fn install_progress_text(progress: &InstallProgress) -> String {
    let label = if progress.label.is_empty() {
        gettextrs::gettext("Installing…")
    } else {
        progress.label.clone()
    };
    match progress.fraction() {
        Some(fraction) => format!("{label} · {}%", (fraction * 100.0).round() as u32),
        None => label,
    }
}
