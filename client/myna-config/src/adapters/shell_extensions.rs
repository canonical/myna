//! gnome-shell's extension list over `org.gnome.Shell.Extensions` on the
//! session bus.
//!
//! gnome-shell scans the extension directories at login only, so what it
//! reports is cross-checked against the disk: a system copy it does not list
//! was installed after login, unless a user copy of the same uuid hides it.
//! Such a copy is enabled through the user's `org.gnome.shell` settings,
//! which gnome-shell reads at the next login.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use gio::glib::{self, Variant, VariantDict, VariantTy};
use gio::prelude::*;

use crate::onboarding::{
    extension_state, ExtensionCopies, ExtensionCopy, ExtensionInfo, ExtensionListing,
    ExtensionReport, ExtensionRun, ExtensionState,
};
use crate::ports::{ShellExtensions, SystemConfiguratorError};

const SHELL_NAME: &str = "org.gnome.Shell";
const SHELL_PATH: &str = "/org/gnome/Shell";
const EXTENSIONS_INTERFACE: &str = "org.gnome.Shell.Extensions";
const SHELL_SCHEMA: &str = "org.gnome.shell";
const ENABLED_KEY: &str = "enabled-extensions";
const DISABLED_KEY: &str = "disabled-extensions";
const EXTENSIONS_OFF_KEY: &str = "disable-user-extensions";
/// A shell that does not answer within this is treated as absent.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// gnome-shell starts an extension once its `enabled-extensions` setting
/// changes, after `EnableExtension` returns.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// `ExtensionType.SYSTEM` in gnome-shell's `extensionUtils.js`.
const TYPE_SYSTEM: f64 = 1.0;
/// `ExtensionState.ACTIVE` and `ACTIVATING`.
const STATE_ENABLED: i64 = 1;
const STATE_ACTIVATING: i64 = 8;
/// `ExtensionState.OUT_OF_DATE`.
const STATE_OUT_OF_DATE: i64 = 4;
/// `ExtensionState.ERROR`.
const STATE_ERROR: i64 = 3;

pub struct GnomeShellExtensions {
    connection: Option<gio::DBusConnection>,
    data_dirs: Vec<PathBuf>,
    user_data_dir: PathBuf,
    settle_timeout: Duration,
    /// `org.gnome.shell`, where gnome-shell is installed.
    shell_settings: Option<gio::Settings>,
}

impl GnomeShellExtensions {
    /// The session bus and the system data directories.
    pub fn new() -> Self {
        Self {
            connection: None,
            data_dirs: glib::system_data_dirs(),
            user_data_dir: glib::user_data_dir(),
            settle_timeout: SETTLE_TIMEOUT,
            shell_settings: gio::SettingsSchemaSource::default()
                .and_then(|source| source.lookup(SHELL_SCHEMA, true))
                .map(|schema| {
                    gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
                }),
        }
    }

    /// gnome-shell's `org.gnome.shell` settings, or none for a machine
    /// without gnome-shell.
    pub fn with_shell_settings(mut self, settings: Option<gio::Settings>) -> Self {
        self.shell_settings = settings;
        self
    }

    /// How long enabling waits for gnome-shell to run the extension.
    pub fn with_settle_timeout(mut self, timeout: Duration) -> Self {
        self.settle_timeout = timeout;
        self
    }

    pub fn with_connection(
        connection: gio::DBusConnection,
        data_dirs: Vec<PathBuf>,
        user_data_dir: PathBuf,
    ) -> Self {
        Self {
            connection: Some(connection),
            data_dirs,
            user_data_dir,
            settle_timeout: SETTLE_TIMEOUT,
            shell_settings: None,
        }
    }

    async fn call(&self, method: &str, uuid: &str, reply: &str) -> Result<Variant, glib::Error> {
        self.call_on(EXTENSIONS_INTERFACE, method, (uuid,).to_variant(), reply)
            .await
    }

    async fn call_on(
        &self,
        interface: &str,
        method: &str,
        parameters: Variant,
        reply: &str,
    ) -> Result<Variant, glib::Error> {
        let connection = match &self.connection {
            Some(connection) => connection.clone(),
            None => gio::bus_get_future(gio::BusType::Session).await?,
        };
        connection
            .call_future(
                // A peer-to-peer connection has no bus to route by name.
                connection
                    .flags()
                    .contains(gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION)
                    .then_some(SHELL_NAME),
                SHELL_PATH,
                interface,
                method,
                Some(&parameters),
                Some(VariantTy::new(reply).expect("valid type")),
                gio::DBusCallFlags::NO_AUTO_START,
                CALL_TIMEOUT.as_millis() as i32,
            )
            .await
    }

    async fn raw_info(&self, uuid: &str) -> Option<Variant> {
        self.call("GetExtensionInfo", uuid, "(a{sv})")
            .await
            .map_err(|error| {
                glib::g_debug!(crate::LOG_DOMAIN, "no extension info for {uuid}: {error}");
            })
            .ok()
            .map(|reply| reply.child_value(0))
    }

    async fn info(&self, uuid: &str) -> Option<ExtensionInfo> {
        let info = self.raw_info(uuid).await?;
        parse_info(&info, self.user_extensions_enabled().await)
    }

    /// The Extensions app's switch. A shell that does not say is taken as
    /// running extensions.
    async fn user_extensions_enabled(&self) -> bool {
        self.call_on(
            "org.freedesktop.DBus.Properties",
            "Get",
            (EXTENSIONS_INTERFACE, "UserExtensionsEnabled").to_variant(),
            "(v)",
        )
        .await
        .ok()
        .and_then(|reply| reply.child_value(0).as_variant()?.get::<bool>())
        .unwrap_or(true)
    }

    /// What the user's settings say gnome-shell does with `uuid` at login.
    fn listing(&self, uuid: &str) -> ExtensionListing {
        let Some(settings) = &self.shell_settings else {
            return ExtensionListing::Unknown;
        };
        if settings.boolean(EXTENSIONS_OFF_KEY) {
            return ExtensionListing::TurnedOff;
        }
        let lists = |key| settings.strv(key).iter().any(|listed| listed == uuid);
        if lists(ENABLED_KEY) && !lists(DISABLED_KEY) {
            ExtensionListing::Enabled
        } else if settings.is_writable(ENABLED_KEY) {
            ExtensionListing::Unlisted
        } else {
            ExtensionListing::Locked
        }
    }

    /// List `uuid` for the next login as `EnableExtension` would: into
    /// `enabled-extensions`, out of `disabled-extensions`.
    fn list_for_login(
        &self,
        settings: &gio::Settings,
        uuid: &str,
    ) -> Result<(), SystemConfiguratorError> {
        let write = |key: &str, edit: &dyn Fn(&mut Vec<String>)| {
            let mut listed: Vec<String> = settings
                .strv(key)
                .iter()
                .map(|listed| listed.to_string())
                .collect();
            let before = listed.clone();
            edit(&mut listed);
            if listed == before {
                return Ok(());
            }
            settings.set_strv(key, listed).map_err(|error| {
                SystemConfiguratorError::setting_execution(
                    format!("{SHELL_SCHEMA} {key}"),
                    error.to_string(),
                )
            })
        };
        if !settings.is_writable(ENABLED_KEY) {
            return Err(SystemConfiguratorError::setting_execution(
                format!("{SHELL_SCHEMA} {ENABLED_KEY}"),
                "the administrator does not let it change",
            ));
        }
        write(ENABLED_KEY, &|listed| {
            if !listed.iter().any(|listed| listed == uuid) {
                listed.push(uuid.to_owned());
            }
        })?;
        write(DISABLED_KEY, &|listed| {
            listed.retain(|listed| listed != uuid)
        })?;
        gio::Settings::sync();
        Ok(())
    }

    fn copies_on_disk(&self, uuid: &str) -> ExtensionCopies {
        let has_copy = |dir: &PathBuf| {
            dir.join("gnome-shell/extensions")
                .join(uuid)
                .join("metadata.json")
                .is_file()
        };
        ExtensionCopies {
            system: self.data_dirs.iter().any(has_copy),
            user: has_copy(&self.user_data_dir),
        }
    }
}

impl Default for GnomeShellExtensions {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl ShellExtensions for GnomeShellExtensions {
    async fn extension_state(&self, uuid: &str) -> ExtensionState {
        let info = self.info(uuid).await;
        extension_state(info, self.copies_on_disk(uuid), self.listing(uuid))
    }

    /// `EnableExtension` for a copy gnome-shell lists; for a system copy it
    /// has not scanned, the listing it starts at the next login.
    async fn enable_extension(&self, uuid: &str) -> Result<(), SystemConfiguratorError> {
        let scanned = matches!(
            self.info(uuid).await,
            Some(ExtensionInfo { system: true, .. })
        );
        let on_disk = self.copies_on_disk(uuid);
        if let (false, true, false, Some(settings)) =
            (scanned, on_disk.system, on_disk.user, &self.shell_settings)
        {
            return self.list_for_login(settings, uuid);
        }
        let failed =
            |message: String| SystemConfiguratorError::dbus_execution(enable_call(uuid), message);
        let reply = self
            .call("EnableExtension", uuid, "(b)")
            .await
            .map_err(|error| failed(error.message().to_owned()))?;
        if !reply.child_value(0).get::<bool>().unwrap_or(false) {
            return Err(failed(format!("gnome-shell does not know {uuid}")));
        }
        let deadline = std::time::Instant::now() + self.settle_timeout;
        loop {
            let reply = self.raw_info(uuid).await;
            // Only a running extension counts: one still activating may yet
            // error.
            let run = match reply.as_ref().and_then(raw_state) {
                Some(STATE_ENABLED) => Some(ExtensionRun::Enabled),
                Some(STATE_ACTIVATING) => None,
                _ => reply
                    .as_ref()
                    .and_then(|info| parse_info(info, true))
                    .map(|info| info.run),
            };
            match run {
                Some(ExtensionRun::Enabled) => return Ok(()),
                Some(
                    ExtensionRun::Failed
                    | ExtensionRun::OutOfDate
                    | ExtensionRun::Locked
                    | ExtensionRun::TurnedOff,
                ) => {
                    let error = reply
                        .as_ref()
                        .and_then(|info| {
                            VariantDict::new(Some(info))
                                .lookup::<String>("error")
                                .ok()
                                .flatten()
                        })
                        .filter(|error| !error.is_empty())
                        .unwrap_or_else(|| "no reason given".to_owned());
                    return Err(failed(format!("gnome-shell could not run {uuid}: {error}")));
                }
                _ if std::time::Instant::now() >= deadline => {
                    return Err(failed(format!(
                        "gnome-shell did not start {uuid} within {} s",
                        self.settle_timeout.as_secs_f32()
                    )));
                }
                _ => glib::timeout_future(SETTLE_POLL).await,
            }
        }
    }
}

/// One `GetExtensionInfo` reply. An unknown uuid is an empty dictionary.
/// gnome-shell sends `type` and `state` as doubles.
pub fn parse_info(info: &Variant, user_extensions_enabled: bool) -> Option<ExtensionInfo> {
    let kind = VariantDict::new(Some(info)).lookup::<f64>("type").ok()??;
    let state = raw_state(info)?;
    // gnome-shell's `_updateCanChange`: false when the administrator locks
    // `enabled-extensions`, or when the user turned extensions off.
    let can_change = VariantDict::new(Some(info))
        .lookup::<bool>("canChange")
        .ok()
        .flatten()
        .unwrap_or(true);
    // `ExtensionState` in gnome-shell's `extensionUtils.js`.
    let run = match state {
        // 8 and 7 are ACTIVATING and DEACTIVATING: read where it is heading.
        STATE_ENABLED | STATE_ACTIVATING => ExtensionRun::Enabled,
        2 | 6 | 7 if can_change => ExtensionRun::Disabled,
        2 | 6 | 7 if !user_extensions_enabled => ExtensionRun::TurnedOff,
        2 | 6 | 7 => ExtensionRun::Locked,
        STATE_OUT_OF_DATE => ExtensionRun::OutOfDate,
        // ERROR, UNINSTALLED, and any state this code does not know.
        _ => ExtensionRun::Failed,
    };
    Some(ExtensionInfo {
        system: kind == TYPE_SYSTEM,
        run,
    })
}

fn raw_state(info: &Variant) -> Option<i64> {
    let state = VariantDict::new(Some(info)).lookup::<f64>("state").ok()??;
    Some(state as i64)
}

/// What gnome-shell says about `uuid`, for Diagnostics. Synchronous, like the
/// page's other reads: it refreshes on demand.
pub fn extension_report(uuid: &str) -> ExtensionReport {
    let reply =
        gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|connection| {
            connection.call_sync(
                Some(SHELL_NAME),
                SHELL_PATH,
                EXTENSIONS_INTERFACE,
                "GetExtensionInfo",
                Some(&(uuid,).to_variant()),
                Some(VariantTy::new("(a{sv})").expect("valid type")),
                gio::DBusCallFlags::NO_AUTO_START,
                CALL_TIMEOUT.as_millis() as i32,
                gio::Cancellable::NONE,
            )
        });
    let report = match reply {
        Ok(reply) => parse_report(&reply.child_value(0), &glib::user_data_dir()),
        Err(_) => ExtensionReport::NoShell,
    };
    let extensions = GnomeShellExtensions::new();
    unscanned_report(
        report,
        extensions.copies_on_disk(uuid),
        extensions.listing(uuid),
    )
}

/// gnome-shell's version, for Diagnostics; `None` when it does not answer.
pub fn shell_version() -> Option<String> {
    let reply = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .and_then(|connection| {
            connection.call_sync(
                Some(SHELL_NAME),
                SHELL_PATH,
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&(EXTENSIONS_INTERFACE, "ShellVersion").to_variant()),
                Some(VariantTy::new("(v)").expect("valid type")),
                gio::DBusCallFlags::NO_AUTO_START,
                CALL_TIMEOUT.as_millis() as i32,
                gio::Cancellable::NONE,
            )
        })
        .ok()?;
    reply.child_value(0).as_variant()?.get::<String>()
}

/// A system copy gnome-shell does not know, because it has not scanned it
/// since it was installed, is not "not installed".
fn unscanned_report(
    report: ExtensionReport,
    on_disk: ExtensionCopies,
    listing: ExtensionListing,
) -> ExtensionReport {
    let unscanned = report == ExtensionReport::NotInstalled
        && on_disk.system
        && !on_disk.user
        && listing != ExtensionListing::Unknown;
    if unscanned {
        ExtensionReport::SinceLogin {
            at_next_login: listing == ExtensionListing::Enabled,
        }
    } else {
        report
    }
}

/// One `GetExtensionInfo` reply as Diagnostics reports it.
pub fn parse_report(info: &Variant, user_data_dir: &std::path::Path) -> ExtensionReport {
    let dict = VariantDict::new(Some(info));
    let Some(state) = raw_state(info) else {
        return ExtensionReport::NotInstalled;
    };
    let path: String = dict.lookup("path").ok().flatten().unwrap_or_default();
    let error = (state == STATE_ERROR)
        .then(|| dict.lookup::<String>("error").ok().flatten())
        .flatten()
        .filter(|error| !error.is_empty());
    ExtensionReport::Known {
        state: state_name(state),
        copy: ExtensionCopy::of(std::path::Path::new(&path), user_data_dir),
        error,
    }
}

/// gnome-shell's `ExtensionState` names, as `gnome-extensions info` prints
/// them.
fn state_name(state: i64) -> String {
    match state {
        1 => "active".into(),
        2 => "inactive".into(),
        3 => "error".into(),
        4 => "out of date".into(),
        5 => "downloading".into(),
        6 => "initialized".into(),
        7 => "deactivating".into(),
        8 => "activating".into(),
        99 => "uninstalled".into(),
        other => format!("state {other}"),
    }
}

/// The call as a failure report names it.
fn enable_call(uuid: &str) -> String {
    format!("{EXTENSIONS_INTERFACE}.EnableExtension({uuid:?})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_call_is_reported_as_the_call() {
        let error = SystemConfiguratorError::dbus_execution(
            enable_call(crate::onboarding::SHELL_EXTENSION_UUID),
            "gnome-shell did not start it",
        );
        assert_eq!(
            crate::backend_ui::system_error_details(&error),
            "D-Bus call: org.gnome.Shell.Extensions.EnableExtension(\"myna-shell@canonical.com\")\n\
             Message: gnome-shell did not start it"
        );
    }

    #[test]
    fn an_extension_the_user_may_not_change_is_locked_or_turned_off() {
        let locked = reply(&[
            ("type", 1.0.to_variant()),
            ("state", 2.0.to_variant()),
            ("canChange", false.to_variant()),
        ]);
        assert_eq!(
            parse_info(&locked, true).map(|info| info.run),
            Some(ExtensionRun::Locked)
        );
        // Extensions switched off by the user, not locked down.
        assert_eq!(
            parse_info(&locked, false).map(|info| info.run),
            Some(ExtensionRun::TurnedOff)
        );
        let running = reply(&[
            ("type", 1.0.to_variant()),
            ("state", 1.0.to_variant()),
            ("canChange", false.to_variant()),
        ]);
        assert_eq!(
            parse_info(&running, false).map(|info| info.run),
            Some(ExtensionRun::Enabled)
        );
    }

    #[test]
    fn a_system_copy_the_shell_has_not_scanned_is_reported_as_such() {
        let copies = |system, user| ExtensionCopies { system, user };
        let since = |at_next_login| ExtensionReport::SinceLogin { at_next_login };
        let unscanned =
            |on_disk, listing| unscanned_report(ExtensionReport::NotInstalled, on_disk, listing);
        assert_eq!(
            unscanned(copies(true, false), ExtensionListing::Enabled),
            since(true)
        );
        for listing in [
            ExtensionListing::Unlisted,
            ExtensionListing::Locked,
            ExtensionListing::TurnedOff,
        ] {
            assert_eq!(unscanned(copies(true, false), listing), since(false));
        }
        // No copy, a shadowing user copy, or no gnome-shell settings: what
        // gnome-shell said stands.
        for (on_disk, listing) in [
            (copies(false, false), ExtensionListing::Enabled),
            (copies(true, true), ExtensionListing::Enabled),
            (copies(true, false), ExtensionListing::Unknown),
        ] {
            assert_eq!(unscanned(on_disk, listing), ExtensionReport::NotInstalled);
        }
        assert_eq!(
            unscanned_report(
                ExtensionReport::NoShell,
                copies(true, false),
                ExtensionListing::Enabled
            ),
            ExtensionReport::NoShell
        );
    }

    fn reply(entries: &[(&str, Variant)]) -> Variant {
        let dict = VariantDict::new(None);
        for (key, value) in entries {
            dict.insert_value(key, value);
        }
        dict.end()
    }

    #[test]
    fn the_report_names_the_running_copy_never_its_path() {
        let home = std::path::Path::new("/home/alice/.local/share");
        let report = |path: &str, state: f64| {
            parse_report(
                &reply(&[
                    ("type", 1.0.to_variant()),
                    ("state", state.to_variant()),
                    ("path", path.to_variant()),
                    ("error", "boom at /home/alice/x.js".to_variant()),
                ]),
                home,
            )
        };
        let cases = [
            (
                "/usr/share/gnome/gnome-shell/extensions/myna-shell@canonical.com",
                ExtensionCopy::MynaConfigPackage,
            ),
            (
                "/usr/share/gnome-shell/extensions/myna-shell@canonical.com",
                ExtensionCopy::UbuntuPackage,
            ),
            (
                "/usr/share/ubuntu/gnome-shell/extensions/myna-shell@canonical.com",
                ExtensionCopy::DevelopmentOverride,
            ),
            (
                "/home/alice/.local/share/gnome-shell/extensions/myna-shell@canonical.com",
                ExtensionCopy::UserCopy,
            ),
            (
                "/opt/x/gnome-shell/extensions/myna-shell@canonical.com",
                ExtensionCopy::OtherSystemCopy,
            ),
            ("", ExtensionCopy::OtherSystemCopy),
        ];
        for (path, copy) in cases {
            assert_eq!(
                report(path, 1.0),
                ExtensionReport::Known {
                    state: "active".into(),
                    copy,
                    error: None,
                },
                "{path}"
            );
        }
        assert_eq!(
            report("/usr/share/gnome-shell/extensions/x", 3.0),
            ExtensionReport::Known {
                state: "error".into(),
                copy: ExtensionCopy::UbuntuPackage,
                error: Some("boom at /home/alice/x.js".into()),
            },
            "only the error state carries gnome-shell's error"
        );
        assert_eq!(
            parse_report(&reply(&[]), home),
            ExtensionReport::NotInstalled
        );
    }

    #[test]
    fn an_unknown_uuid_is_no_info() {
        assert_eq!(parse_info(&reply(&[]), true), None);
    }

    #[test]
    fn type_and_state_are_read_as_doubles() {
        let info = |kind: f64, state: f64| {
            parse_info(
                &reply(&[
                    ("type", kind.to_variant()),
                    ("state", state.to_variant()),
                    ("uuid", "myna-shell@canonical.com".to_variant()),
                ]),
                true,
            )
        };
        assert_eq!(
            info(1.0, 1.0),
            Some(ExtensionInfo {
                system: true,
                run: ExtensionRun::Enabled
            })
        );
        assert_eq!(
            info(2.0, 2.0),
            Some(ExtensionInfo {
                system: false,
                run: ExtensionRun::Disabled
            })
        );
        assert_eq!(
            info(1.0, 6.0).map(|info| info.run),
            Some(ExtensionRun::Disabled)
        );
        // Mid-toggle: read the state it is heading for.
        assert_eq!(
            info(1.0, 8.0).map(|info| info.run),
            Some(ExtensionRun::Enabled)
        );
        assert_eq!(
            info(1.0, 7.0).map(|info| info.run),
            Some(ExtensionRun::Disabled)
        );
        assert_eq!(
            info(1.0, 4.0).map(|info| info.run),
            Some(ExtensionRun::OutOfDate)
        );
        for failed in [3.0, 5.0, 99.0] {
            assert_eq!(
                info(1.0, failed).map(|info| info.run),
                Some(ExtensionRun::Failed)
            );
        }
    }

    #[test]
    fn integer_fields_are_not_gnome_shells() {
        let info = reply(&[("type", 1i32.to_variant()), ("state", 1i32.to_variant())]);
        assert_eq!(parse_info(&info, true), None);
    }
}
