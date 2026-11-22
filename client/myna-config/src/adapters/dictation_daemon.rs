//! The daemon's model surface over the session bus: `State`, `InstalledSteps`
//! and `RescanModels` on `com.canonical.Myna.Dictation`.

use async_trait::async_trait;
use gio::glib::{self, variant::ToVariant, VariantDict};

use crate::ports::DictationDaemon;

const BUS: &str = "com.canonical.Myna.Dictation";
const PATH: &str = "/com/canonical/Myna/Dictation";
const TIMEOUT_MS: i32 = 2_000;

/// The daemon on the session bus, or on the bus at `address` (tests).
#[derive(Default)]
pub struct SessionBusDaemon {
    address: Option<String>,
}

impl SessionBusDaemon {
    pub fn on_bus(address: &str) -> Self {
        Self {
            address: Some(address.to_owned()),
        }
    }

    async fn connection(&self) -> Result<gio::DBusConnection, String> {
        match &self.address {
            None => gio::bus_get_future(gio::BusType::Session).await,
            Some(address) => {
                gio::DBusConnection::for_address_future(
                    address,
                    gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                    None,
                )
                .await
            }
        }
        .map_err(|error| error.to_string())
    }
}

/// Whether `error` says no daemon owns the name, which is no failure.
fn not_running(error: &glib::Error) -> bool {
    gio::DBusError::remote_error(error).is_some_and(|name| {
        matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        )
    })
}

async fn call(
    daemon: &SessionBusDaemon,
    interface: &str,
    method: &str,
    parameters: glib::Variant,
) -> Result<Option<glib::Variant>, String> {
    let connection = daemon.connection().await?;
    match connection
        .call_future(
            Some(BUS),
            PATH,
            interface,
            method,
            Some(&parameters),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            TIMEOUT_MS,
        )
        .await
    {
        Ok(reply) => Ok(Some(reply)),
        Err(error) if not_running(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

async fn property(daemon: &SessionBusDaemon, name: &str) -> Result<Option<String>, String> {
    let Some(all) = call(
        daemon,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        (BUS,).to_variant(),
    )
    .await?
    else {
        return Ok(None);
    };
    let dict = VariantDict::new(Some(&all.child_value(0)));
    Ok(Some(
        dict.lookup::<String>(name)
            .ok()
            .flatten()
            .unwrap_or_default(),
    ))
}

/// The model ids of an `InstalledSteps` value; empty for a daemon too old to
/// publish it.
pub fn parse_offered(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| {
            value.get("models")?.as_array().map(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("model")?.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default()
}

#[async_trait(?Send)]
impl DictationDaemon for SessionBusDaemon {
    async fn state(&self) -> Result<Option<String>, String> {
        property(self, "State").await
    }

    async fn offered_models(&self) -> Result<Option<Vec<String>>, String> {
        Ok(property(self, "InstalledSteps")
            .await?
            .map(|json| parse_offered(&json)))
    }

    async fn installed_steps(&self) -> Result<Option<String>, String> {
        property(self, "InstalledSteps").await
    }

    async fn rescan(&self) -> Result<(), String> {
        call(self, BUS, "RescanModels", ().to_variant())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_of_installed_steps_are_listed_and_an_old_daemon_has_none() {
        let json = r#"{"models":[{"share":"s","model":"a"},{"model":"b"},{"x":1}],"steps":[]}"#;
        assert_eq!(parse_offered(json), ["a", "b"]);
        assert!(parse_offered("").is_empty());
        assert!(parse_offered("{}").is_empty());
    }

    #[test]
    fn a_missing_name_owner_is_not_running_and_anything_else_is_not() {
        let named = gio::DBusError::new_for_dbus_error(
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "no owner",
        );
        assert!(not_running(&named));
        let coded = glib::Error::new(gio::DBusError::NameHasNoOwner, "no owner");
        assert!(not_running(&coded));
        assert!(!not_running(&glib::Error::new(
            gio::DBusError::Failed,
            "boom"
        )));
    }
}
