//! Appearance from the settings portal's `org.freedesktop.appearance`
//! namespace, the one source GNOME, Plasma and any other desktop with an
//! `xdg-desktop-portal` backend answer alike.
//!
//! A key the portal states decides; one it does not answer, or answers with
//! "no preference", is the fallback's. Xubuntu's portal has no Settings
//! interface at all, so there the fallback is the whole answer. Values are
//! cached from `ReadAll` and kept current by `SettingChanged`, so readings
//! taken in a watch callback are already current.

use std::cell::RefCell;
use std::rc::Rc;

use gio::glib::Variant;
use gio::prelude::*;
use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness, Rgb};
use myna_platform::Subscription;

pub const SERVICE: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const INTERFACE: &str = "org.freedesktop.portal.Settings";
pub const NAMESPACE: &str = "org.freedesktop.appearance";

/// Whose accent wins when both have one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccentFrom {
    /// The portal's exact colour (Plasma's own accent; libadwaita would round
    /// it to the nearest of its nine).
    Portal,
    /// The fallback's. Ubuntu's libadwaita and GNOME Shell tint a named
    /// accent with Yaru's shade, which the GNOME portal does not report.
    Fallback,
}

/// What the portal says, key by key; `None` where it states no preference.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Answers {
    accent: Option<Rgb>,
    reduced_motion: Option<bool>,
    high_contrast: Option<bool>,
    prefers_dark: Option<bool>,
}

impl Answers {
    /// Take in one key's value as the portal documents it.
    fn take(&mut self, key: &str, value: &Variant) {
        let number = value.get::<u32>();
        match key {
            "accent-color" => self.accent = value.get::<(f64, f64, f64)>().and_then(in_gamut),
            "color-scheme" => {
                self.prefers_dark = match number {
                    Some(1) => Some(true),
                    Some(2) => Some(false),
                    _ => None,
                }
            }
            "contrast" => self.high_contrast = (number == Some(1)).then_some(true),
            "reduced-motion" => self.reduced_motion = (number == Some(1)).then_some(true),
            _ => {}
        }
    }

    /// The answers in an `a{sv}` of the namespace.
    fn from_namespace(settings: &Variant) -> Self {
        let mut answers = Self::default();
        for entry in settings.iter() {
            if let (Some(key), Some(value)) = (
                entry.child_value(0).str(),
                entry.child_value(1).as_variant(),
            ) {
                answers.take(key, &value);
            }
        }
        answers
    }
}

/// The spec's out-of-range accent is an unset one.
fn in_gamut((r, g, b): (f64, f64, f64)) -> Option<Rgb> {
    [r, g, b]
        .iter()
        .all(|c| (0.0..=1.0).contains(c))
        .then_some(Rgb { r, g, b })
}

pub struct PortalAppearance {
    connection: gio::DBusConnection,
    answers: Rc<RefCell<Answers>>,
    fallback: Rc<dyn Appearance>,
    accent: AccentFrom,
    /// Keeps `answers` current while nothing watches.
    _updates: gio::SignalSubscription,
}

impl PortalAppearance {
    /// The portal on `connection` over `fallback`, or `fallback` alone when
    /// no portal answers the namespace.
    pub fn open(
        connection: &gio::DBusConnection,
        fallback: Rc<dyn Appearance>,
        accent: AccentFrom,
    ) -> Rc<dyn Appearance> {
        let reply = crate::bus::call(
            connection,
            SERVICE,
            PATH,
            INTERFACE,
            "ReadAll",
            Variant::tuple_from_iter([[NAMESPACE].to_variant()]),
            "(a{sa{sv}})",
        );
        let Some(settings) = reply.ok().and_then(|reply| {
            reply
                .child_value(0)
                .iter()
                .find(|entry| entry.child_value(0).str() == Some(NAMESPACE))
                .map(|entry| entry.child_value(1))
        }) else {
            return fallback;
        };
        let answers = Rc::new(RefCell::new(Answers::from_namespace(&settings)));
        let updates = follow(connection, answers.clone(), || {});
        Rc::new(Self {
            connection: connection.clone(),
            answers,
            fallback,
            accent,
            _updates: updates,
        })
    }

    /// [`PortalAppearance::open`] on the session bus.
    pub fn session(fallback: Rc<dyn Appearance>, accent: AccentFrom) -> Rc<dyn Appearance> {
        match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(connection) => Self::open(&connection, fallback, accent),
            Err(_) => fallback,
        }
    }
}

/// Keep `answers` current, calling `changed` after each change.
fn follow(
    connection: &gio::DBusConnection,
    answers: Rc<RefCell<Answers>>,
    changed: impl Fn() + 'static,
) -> gio::SignalSubscription {
    connection.subscribe_to_signal(
        connection
            .flags()
            .contains(gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION)
            .then_some(SERVICE),
        Some(INTERFACE),
        Some("SettingChanged"),
        Some(PATH),
        Some(NAMESPACE),
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let parameters = signal.parameters;
            let (Some(key), Some(value)) = (
                parameters.child_value(1).str().map(str::to_owned),
                parameters.child_value(2).as_variant(),
            ) else {
                return;
            };
            answers.borrow_mut().take(&key, &value);
            changed();
        },
    )
}

impl Appearance for PortalAppearance {
    fn read(&self) -> AppearanceReadings {
        let fallback = self.fallback.read();
        let answers = *self.answers.borrow();
        AppearanceReadings {
            accent: match self.accent {
                AccentFrom::Portal => answers.accent.or(fallback.accent),
                AccentFrom::Fallback => fallback.accent.or(answers.accent),
            },
            reduced_motion: answers.reduced_motion.unwrap_or(fallback.reduced_motion),
            high_contrast: answers.high_contrast.unwrap_or(fallback.high_contrast),
            prefers_dark: answers.prefers_dark.unwrap_or(fallback.prefers_dark),
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: Rc<dyn Fn(Freshness)> = changed.into();
        let portal = follow(&self.connection, self.answers.clone(), {
            let changed = changed.clone();
            move || changed(Freshness::Current)
        });
        let fallback = self
            .fallback
            .watch(Box::new(move |freshness| changed(freshness)));
        Subscription::new(move || {
            drop(portal);
            drop(fallback);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(entries: &[(&str, Variant)]) -> Answers {
        let dict = gio::glib::VariantDict::new(None);
        for (key, value) in entries {
            dict.insert_value(key, value);
        }
        Answers::from_namespace(&dict.end())
    }

    #[test]
    fn each_key_is_read_as_the_portal_documents_it() {
        let read = answers(&[
            ("accent-color", (0.24, 0.68, 0.91).to_variant()),
            ("color-scheme", 1u32.to_variant()),
            ("contrast", 1u32.to_variant()),
            ("reduced-motion", 1u32.to_variant()),
        ]);
        assert_eq!(
            read,
            Answers {
                accent: Some(Rgb {
                    r: 0.24,
                    g: 0.68,
                    b: 0.91
                }),
                reduced_motion: Some(true),
                high_contrast: Some(true),
                prefers_dark: Some(true),
            }
        );
        assert_eq!(
            answers(&[("color-scheme", 2u32.to_variant())]).prefers_dark,
            Some(false)
        );
    }

    #[test]
    fn no_preference_is_left_to_the_fallback() {
        let read = answers(&[
            ("color-scheme", 0u32.to_variant()),
            ("contrast", 0u32.to_variant()),
            ("reduced-motion", 0u32.to_variant()),
        ]);
        assert_eq!(read, Answers::default());
    }

    #[test]
    fn an_accent_out_of_gamut_is_unset() {
        for accent in [(-1.0, -1.0, -1.0), (0.5, 1.2, 0.5), (0.0, 0.0, f64::NAN)] {
            assert_eq!(
                answers(&[("accent-color", accent.to_variant())]).accent,
                None,
                "{accent:?}"
            );
        }
        assert!(answers(&[("accent-color", (0.0, 1.0, 0.0).to_variant())])
            .accent
            .is_some());
    }

    #[test]
    fn values_of_the_wrong_type_and_unknown_keys_say_nothing() {
        let read = answers(&[
            ("accent-color", "#3daee9".to_variant()),
            ("color-scheme", "prefer-dark".to_variant()),
            ("contrast", true.to_variant()),
            ("cursor-size", 24u32.to_variant()),
        ]);
        assert_eq!(read, Answers::default());
    }

    #[test]
    fn a_later_value_replaces_an_earlier_one() {
        let mut read = answers(&[("color-scheme", 1u32.to_variant())]);
        read.take("color-scheme", &0u32.to_variant());
        assert_eq!(read.prefers_dark, None);
        read.take("reduced-motion", &1u32.to_variant());
        read.take("reduced-motion", &0u32.to_variant());
        assert_eq!(read.reduced_motion, None);
    }
}
