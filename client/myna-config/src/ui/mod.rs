use std::sync::Once;

use gtk::gio;
use gtk4 as gtk;

mod backend_page;
mod balanced_label;
mod diagnostics_page;
mod install_control;
mod install_models_dialog;
mod main_window;
mod myna_page;
mod onboarding_components;
mod onboarding_shortcut;
mod onboarding_welcome;
mod onboarding_window;
mod operation_error_dialog;
mod status_page;

pub use backend_page::BackendPage;
pub use balanced_label::BalancedLabel;
pub use diagnostics_page::DiagnosticsPage;
pub use install_control::{installing_text, InstallControl, RowProgress};
pub use install_models_dialog::InstallModelsDialog;
pub use main_window::MainWindow;
pub use myna_page::MynaPage;
pub use onboarding_components::{ComponentsStatus, OnboardingComponents};
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

/// GTK binds to the first thread that initializes it and libtest gives every
/// test its own, so GTK tests share gtk's one test thread. Gated like the
/// probes: `ui-check` and `cov` run them under Xvfb.
#[cfg(test)]
pub(crate) fn on_gtk_thread(test: impl FnOnce() + Send + std::panic::UnwindSafe + 'static) {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }
    // A Wayland session keeps WAYLAND_DISPLAY even under xvfb-run, and GTK
    // then paints on the developer's compositor instead of Xvfb - and reads
    // that session's portal, which is why such a run disagrees with CI. The
    // login session's runtime dir is what makes it the real desktop: a
    // headless compositor (dev/headless-gtk.sh) runs on a private one.
    let desktop_compositor = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_RUNTIME_DIR").is_ok_and(|dir| dir.starts_with("/run/user/"));
    if desktop_compositor {
        panic!("GTK tests would open windows on this desktop; run dev/headless-gtk.sh (--wayland for the Wayland backend)");
    }
    gtk::test_synced(test);
}
