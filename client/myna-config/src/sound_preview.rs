//! The Sound style row's Preview: asks the daemon to play a sound set over
//! D-Bus (`PreviewSounds`), because the cues are compiled into the daemon and
//! it alone knows whether a session has the microphone open.
//!
//! Read through `gio`'s D-Bus like the diagnostics report (`machine.rs`):
//! `gio` is already a dependency, and the call is one round trip.

use gio::glib;
use gio::prelude::*;

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";

/// The daemon's own refusals, as it names them on the wire.
const ERROR_PREFIX: &str = "com.canonical.Myna.Dictation.Error.";

/// Why a preview did not play, for the user. A preview that is already
/// playing is not one: the user hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewRefusal {
    /// No daemon owns the name.
    NotRunning,
    /// A daemon older than the method.
    Outdated,
    /// A session is under way and the microphone would hear the preview.
    Dictating,
    /// Anything else, with the raw error for the log.
    Failed(String),
}

impl PreviewRefusal {
    /// The refusal behind a failed call; `None` when the user hears a preview
    /// anyway.
    pub fn of(error: &glib::Error) -> Option<Self> {
        // A bus error arrives either as its D-Bus name or, once GLib has
        // registered the bus's own names, as a `GDBusError` code; read both.
        let remote = gio::DBusError::remote_error(error);
        let code = error.kind::<gio::DBusError>();
        let name = remote.as_deref().or(match code {
            Some(gio::DBusError::ServiceUnknown) => {
                Some("org.freedesktop.DBus.Error.ServiceUnknown")
            }
            Some(gio::DBusError::NameHasNoOwner) => {
                Some("org.freedesktop.DBus.Error.NameHasNoOwner")
            }
            Some(gio::DBusError::UnknownMethod) => Some("org.freedesktop.DBus.Error.UnknownMethod"),
            _ => None,
        });
        match name {
            Some(
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner",
            ) => Some(Self::NotRunning),
            Some("org.freedesktop.DBus.Error.UnknownMethod") => Some(Self::Outdated),
            Some(name) => match name.strip_prefix(ERROR_PREFIX) {
                Some("AlreadyPlaying") => None,
                Some("Busy") => Some(Self::Dictating),
                _ => Some(Self::Failed(error.to_string())),
            },
            None => Some(Self::Failed(error.to_string())),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotRunning => gettextrs::gettext("Myna is not running"),
            Self::Outdated => gettextrs::gettext("Update Myna to preview sounds"),
            Self::Dictating => gettextrs::gettext("Finish dictating to preview sounds"),
            Self::Failed(_) => gettextrs::gettext("Could not preview sounds"),
        }
    }
}

/// Ask the daemon to play `set` (a `sound-set` nick), calling `done` on this
/// thread's main context with the refusal, if any.
pub fn request(set: &str, done: impl FnOnce(Option<PreviewRefusal>) + 'static) {
    let set = set.to_owned();
    glib::MainContext::default().spawn_local(async move {
        let reply = match gio::bus_get_future(gio::BusType::Session).await {
            Ok(connection) => {
                connection
                    .call_future(
                        Some(DICTATION_BUS),
                        DICTATION_PATH,
                        DICTATION_BUS,
                        "PreviewSounds",
                        Some(&(set.as_str(),).to_variant()),
                        None,
                        gio::DBusCallFlags::NO_AUTO_START,
                        2_000,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        done(reply.err().and_then(|error| PreviewRefusal::of(&error)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(name: &str) -> Option<PreviewRefusal> {
        PreviewRefusal::of(&gio::DBusError::new_for_dbus_error(name, "message"))
    }

    /// The same bus errors once GLib maps them to `GDBusError` codes.
    #[test]
    fn bus_error_codes_read_like_their_names() {
        let coded = |code| PreviewRefusal::of(&glib::Error::new(code, "message"));
        assert_eq!(
            coded(gio::DBusError::ServiceUnknown),
            Some(PreviewRefusal::NotRunning)
        );
        assert_eq!(
            coded(gio::DBusError::NameHasNoOwner),
            Some(PreviewRefusal::NotRunning)
        );
        assert_eq!(
            coded(gio::DBusError::UnknownMethod),
            Some(PreviewRefusal::Outdated)
        );
        assert!(matches!(
            coded(gio::DBusError::Timeout),
            Some(PreviewRefusal::Failed(_))
        ));
        assert!(matches!(
            PreviewRefusal::of(&glib::Error::new(gio::IOErrorEnum::NotFound, "no bus")),
            Some(PreviewRefusal::Failed(_))
        ));
    }

    #[test]
    fn a_missing_daemon_reads_as_not_running() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert_eq!(refusal(name), Some(PreviewRefusal::NotRunning), "{name}");
        }
    }

    #[test]
    fn a_daemon_without_the_method_reads_as_outdated() {
        assert_eq!(
            refusal("org.freedesktop.DBus.Error.UnknownMethod"),
            Some(PreviewRefusal::Outdated)
        );
    }

    #[test]
    fn the_daemons_own_refusals_are_told_apart_by_name() {
        assert_eq!(
            refusal("com.canonical.Myna.Dictation.Error.Busy"),
            Some(PreviewRefusal::Dictating)
        );
        assert_eq!(
            refusal("com.canonical.Myna.Dictation.Error.AlreadyPlaying"),
            None
        );
        for name in [
            "com.canonical.Myna.Dictation.Error.NoPlayer",
            "com.canonical.Myna.Dictation.Error.UnknownSoundSet",
            "org.freedesktop.DBus.Error.AccessDenied",
            "com.example.Unheard",
        ] {
            assert!(
                matches!(refusal(name), Some(PreviewRefusal::Failed(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn every_refusal_has_its_own_message() {
        let messages: Vec<_> = [
            PreviewRefusal::NotRunning,
            PreviewRefusal::Outdated,
            PreviewRefusal::Dictating,
            PreviewRefusal::Failed(String::new()),
        ]
        .iter()
        .map(PreviewRefusal::message)
        .collect();
        for (i, message) in messages.iter().enumerate() {
            assert!(!message.is_empty());
            assert!(!messages[i + 1..].contains(message), "{message}");
        }
    }
}
