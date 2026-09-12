//! The HUD-style forwarder: carries the `hud-style` setting to the renderer
//! over `com.canonical.Myna.Dictation` (contract dbus-interface.md, the
//! `HudStyle` property).
//!
//! ## Why the daemon carries a setting it has no opinion about
//!
//! The HUD used to read `hud-style` itself, through a plain
//! `gio::Settings::new()` — i.e. through whatever backend its environment
//! selected. That is not the backend the rest of the client uses. Every other
//! app that touches the store runs with `GSETTINGS_BACKEND=keyfile` (see the
//! snap's `*settings-env`), while `myna.hud` deliberately keeps the GNOME
//! extension's default backend so it can still read the host's
//! `org.gnome.desktop.interface` accent and animation preferences.
//!
//! So `myna.config set hud-style vumeter` wrote the keyfile store and the HUD
//! read dconf, found nothing, and silently rendered the schema default. Both
//! halves were individually right; the key was simply on the wrong side of a
//! boundary drawn for host settings.
//!
//! The fix is not to reconcile the two backends but to remove the second
//! reader. Settings have exactly one reader — the daemon — and the HUD is a
//! renderer that is *told* what to draw, over the channel it already consumes
//! everything else on. A confined publisher's `PropertiesChanged` is that
//! channel (contract §Confinement); no new interface, no new permission.

use tokio::sync::watch;

use crate::dbus::{PropertyValue, SharedBus};

/// The property the nick is published under.
pub const PROPERTY: &str = "HudStyle";

/// Publish the current `hud-style` nick, then every change, until the sender
/// is dropped (daemon shutdown).
///
/// The initial publish is unconditional: a HUD that connects before the first
/// settings change must still learn the style, and the served snapshot it
/// reads on name-appeared is what this keeps honest.
pub async fn run(bus: SharedBus, mut styles: watch::Receiver<String>) {
    // Each read is scoped: `watch::Ref` holds a lock guard, and holding one
    // across the publish await would make this future non-`Send` (and hold the
    // settings cell for the length of a bus round trip).
    let nick = styles.borrow_and_update().clone();
    publish(&bus, nick).await;
    while styles.changed().await.is_ok() {
        let nick = styles.borrow_and_update().clone();
        publish(&bus, nick).await;
    }
}

async fn publish(bus: &SharedBus, nick: String) {
    bus.lock()
        .await
        .set_property(PROPERTY, PropertyValue::Str(nick))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::{DictationService, FakeBus};

    /// The forwarder publishes the value it starts with, without waiting for
    /// a change — a HUD that connects first must not render the wrong meter
    /// until the user happens to edit the setting.
    #[tokio::test]
    async fn publishes_the_initial_style() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel("vumeter".to_string());

        let handle = tokio::spawn(run(service.bus(), rx));
        tokio::task::yield_now().await;

        assert_eq!(
            fake.property(PROPERTY),
            Some(PropertyValue::Str("vumeter".into()))
        );
        drop(tx);
        handle.await.unwrap();
    }

    /// A settings change reaches the bus, so the HUD swaps meters without a
    /// restart of either process.
    #[tokio::test]
    async fn forwards_every_change() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel("bar".to_string());

        let handle = tokio::spawn(run(service.bus(), rx));
        tokio::task::yield_now().await;

        tx.send_replace("ribbon".to_string());
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert_eq!(
            fake.property(PROPERTY),
            Some(PropertyValue::Str("ribbon".into()))
        );
        drop(tx);
        handle.await.unwrap();
    }

    /// It carries the nick verbatim: validating it is the schema's job on the
    /// way in and the renderer's on the way out, and a daemon that "corrected"
    /// an unfamiliar value would make a newer HUD unable to gain a new style.
    #[tokio::test]
    async fn carries_an_unknown_nick_verbatim() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel("hologram".to_string());

        let handle = tokio::spawn(run(service.bus(), rx));
        tokio::task::yield_now().await;

        assert_eq!(
            fake.property(PROPERTY),
            Some(PropertyValue::Str("hologram".into()))
        );
        drop(tx);
        handle.await.unwrap();
    }

    /// It touches nothing else — the state machine and the level pump own
    /// their own properties.
    #[tokio::test]
    async fn publishes_only_the_style() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel("bar".to_string());
        let handle = tokio::spawn(run(service.bus(), rx));
        tokio::task::yield_now().await;
        drop(tx);
        handle.await.unwrap();

        assert!(fake.property("State").is_none());
        assert!(fake.property("AudioRms").is_none());
        assert!(fake.property("StatusMessage").is_none());
    }
}
