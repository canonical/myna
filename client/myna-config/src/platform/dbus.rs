//! The session bus for the activation backends, whose calls are bounded: a
//! service that hangs (KWin hosts kglobalaccel) must not freeze Settings.

use gio::glib;
use myna_platform::activation::ActivationError;

pub use myna_freedesktop::bus::call;

pub fn session() -> Result<gio::DBusConnection, ActivationError> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| unavailable("the session bus", &error))
}

pub fn unavailable(what: &str, error: &glib::Error) -> ActivationError {
    ActivationError::Unavailable(format!("{what}: {}", error.message()))
}
