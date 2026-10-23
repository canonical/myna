//! Synchronous calls to a desktop's services on the session bus, each bounded:
//! a service that hangs (KWin hosts kglobalaccel) must not freeze Settings.

use std::time::Duration;

use gio::glib::{self, Variant, VariantTy};
use myna_platform::activation::ActivationError;

/// A service that does not answer in this long is as good as absent.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn session() -> Result<gio::DBusConnection, ActivationError> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| unavailable("the session bus", &error))
}

/// `method` of `service`, whose reply has type `reply`.
pub fn call(
    connection: &gio::DBusConnection,
    service: &str,
    path: &str,
    interface: &str,
    method: &str,
    parameters: Variant,
    reply: &str,
) -> Result<Variant, glib::Error> {
    connection.call_sync(
        // A peer-to-peer connection has no bus to route by name.
        connection
            .flags()
            .contains(gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION)
            .then_some(service),
        path,
        interface,
        method,
        Some(&parameters),
        Some(VariantTy::new(reply).expect("valid type")),
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT.as_millis() as i32,
        gio::Cancellable::NONE,
    )
}

pub fn unavailable(what: &str, error: &glib::Error) -> ActivationError {
    ActivationError::Unavailable(format!("{what}: {}", error.message()))
}
