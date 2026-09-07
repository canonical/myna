//! signals — turn `SIGINT`/`SIGTERM` into an application quit (T124).
//!
//! A signal is how the HUD is stopped: the host retires its client, or a
//! terminal interrupts a dev run. The default disposition kills the process
//! mid-frame, leaving `com.canonical.Myna.Hud` held until the bus notices,
//! skipping `shutdown`, and losing the profile an instrumented run writes on
//! the way out through `main`.

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

/// Route the first `SIGINT`/`SIGTERM` to [`gio::Application::quit`].
///
/// The watch is a GLib main-loop source, so the quit runs on the main thread
/// between frames rather than inside a signal handler. GLib restores the
/// default disposition once its last watch for a signal is gone, and these
/// watches are one-shot: a second signal after the first has been dispatched
/// terminates the process, so a shutdown that hangs is still interruptible.
pub fn quit_on_signal() {
    for signum in [libc::SIGINT, libc::SIGTERM] {
        glib_unix::unix_signal_add_once(signum, || {
            if let Some(app) = gio::Application::default() {
                app.quit();
            }
        });
    }
}
