use std::sync::Once;

use gtk::gio;
use gtk4 as gtk;

mod active_backend_dialog;
mod apply_dialog;
mod backend_apply_controls;
mod backend_page;
mod diagnostics_page;
mod main_window;
mod myna_page;
mod onboarding_components;
mod onboarding_shortcut;
mod onboarding_welcome;
mod onboarding_window;
mod operation_error_dialog;
mod status_page;

pub use active_backend_dialog::ActiveBackendDialog;
pub use apply_dialog::ApplyDialog;
pub use backend_apply_controls::BackendApplyControls;
pub use backend_page::BackendPage;
pub use diagnostics_page::DiagnosticsPage;
pub use main_window::MainWindow;
pub use myna_page::MynaPage;
pub use onboarding_components::OnboardingComponents;
pub use onboarding_shortcut::OnboardingShortcut;
pub use onboarding_welcome::OnboardingWelcome;
pub use onboarding_window::OnboardingWindow;
pub use operation_error_dialog::OperationErrorDialog;
pub use status_page::StatusPage;

static RESOURCES: Once = Once::new();

pub fn register_resources() {
    RESOURCES.call_once(|| {
        gio::resources_register_include!("myna-config.gresource")
            .expect("failed to register myna-config UI resources");
    });
}
