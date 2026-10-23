//! The slice of xfconf's D-Bus API Settings uses, synchronously.
//!
//! `org.xfce.Xfconf` at `/org/xfce/Xfconf`, on the session bus where xfconfd
//! is started on demand. A property is a slash path in a channel and holds a
//! variant.

use gio::glib::{self, Variant};
use gio::prelude::*;
use myna_platform::activation::ActivationError;

use crate::platform::dbus::{self, unavailable};

const SERVICE: &str = "org.xfce.Xfconf";
const PATH: &str = "/org/xfce/Xfconf";
const INTERFACE: &str = "org.xfce.Xfconf";
const NOT_FOUND: [&str; 2] = [
    "org.xfce.Xfconf.Error.PropertyNotFound",
    "org.xfce.Xfconf.Error.ChannelNotFound",
];

#[derive(Clone)]
pub struct Xfconf {
    connection: gio::DBusConnection,
    channel: String,
}

/// Ends the watch when dropped.
pub struct Subscription(#[allow(dead_code)] Vec<gio::SignalSubscription>);

impl Xfconf {
    /// `channel` on the session bus.
    pub fn session(channel: &str) -> Result<Self, ActivationError> {
        Ok(Self::with_connection(dbus::session()?, channel))
    }

    pub fn with_connection(connection: gio::DBusConnection, channel: &str) -> Self {
        Self {
            connection,
            channel: channel.to_owned(),
        }
    }

    /// Every property at or under `base`, sorted by path. A base xfconfd does
    /// not know holds nothing.
    pub fn all(&self, base: &str) -> Result<Vec<(String, Variant)>, ActivationError> {
        let reply = match self.call(
            "GetAllProperties",
            (&self.channel, base).to_variant(),
            "(a{sv})",
        ) {
            Ok(reply) => reply,
            Err(error) if is_not_found(&error) => return Ok(Vec::new()),
            Err(error) => return Err(unavailable("GetAllProperties", &error)),
        };
        let mut found: Vec<(String, Variant)> = reply
            .child_value(0)
            .iter()
            .filter_map(|entry| {
                let property = entry.child_value(0).str()?.to_owned();
                let value = entry.child_value(1).as_variant()?;
                Some((property, value))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// The property's value, `None` when it does not exist.
    pub fn get(&self, property: &str) -> Result<Option<Variant>, ActivationError> {
        match self.call("GetProperty", (&self.channel, property).to_variant(), "(v)") {
            Ok(reply) => Ok(reply.child_value(0).as_variant()),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(unavailable("GetProperty", &error)),
        }
    }

    pub fn set(&self, property: &str, value: &Variant) -> Result<(), ActivationError> {
        let parameters = Variant::tuple_from_iter([
            self.channel.to_variant(),
            property.to_variant(),
            Variant::from_variant(value),
        ]);
        self.call("SetProperty", parameters, "()")
            .map(drop)
            .map_err(|error| ActivationError::Refused(format!("SetProperty {property}: {error}")))
    }

    /// Reset `property` and, with `recursive`, everything under it.
    pub fn reset(&self, property: &str, recursive: bool) -> Result<(), ActivationError> {
        let parameters = (&self.channel, property, recursive).to_variant();
        match self.call("ResetProperty", parameters, "()") {
            Ok(_) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(ActivationError::Refused(format!(
                "ResetProperty {property}: {error}"
            ))),
        }
    }

    /// Call `changed` with the path of each property of this channel that
    /// changes or goes.
    pub fn watch(&self, changed: impl Fn(&str) + Clone + 'static) -> Subscription {
        let subscriptions = ["PropertyChanged", "PropertyRemoved"]
            .into_iter()
            .map(|member| {
                let changed = changed.clone();
                self.connection.subscribe_to_signal(
                    None,
                    Some(INTERFACE),
                    Some(member),
                    Some(PATH),
                    Some(&self.channel),
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        if let Some(property) = signal.parameters.child_value(1).str() {
                            changed(property);
                        }
                    },
                )
            })
            .collect();
        Subscription(subscriptions)
    }

    fn call(&self, method: &str, parameters: Variant, reply: &str) -> Result<Variant, glib::Error> {
        dbus::call(
            &self.connection,
            SERVICE,
            PATH,
            INTERFACE,
            method,
            parameters,
            reply,
        )
    }
}

fn is_not_found(error: &glib::Error) -> bool {
    gio::DBusError::remote_error(error).is_some_and(|name| NOT_FOUND.contains(&name.as_str()))
}
