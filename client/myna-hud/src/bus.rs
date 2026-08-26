//! bus — the D-Bus worker thread (feature 004, T124).
//!
//! GTK owns the main thread, so the session-bus traffic runs on a worker
//! and reaches the UI through a channel. The worker does **no
//! interpretation**: it turns bus activity into [`BusEvent`]s and
//! [`crate::dbus_consumer::DictationService`] — which is contract-tested
//! headless — applies every rule (dormancy, reflect-on-appeared,
//! clear-to-idle, the dedup split).
//!
//! Everything is read from the standard
//! `org.freedesktop.DBus.Properties` interface, never a custom signal,
//! because that is the only push channel a strictly-confined snap publisher
//! is allowed to use (contract `dbus-interface.md` §Confinement).

use std::collections::HashMap;

use zbus::blocking::{fdo::DBusProxy, fdo::PropertiesProxy, Connection};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::OwnedValue;

use crate::dbus_consumer::{Snapshot, BUS_NAME, OBJECT_PATH};

/// The interface whose properties carry the whole UI state (E1–E3).
const INTERFACE: &str = "org.myna.Dictation";

/// What the worker observed. The rules live in the consumer.
#[derive(Clone, Debug)]
pub enum BusEvent {
    /// The publisher appeared (or was already there), with its current
    /// property snapshot.
    NameAppeared(Snapshot),
    /// The publisher exited or crashed.
    NameVanished,
    /// A `PropertiesChanged` push, re-read from the proxy's cache.
    Properties(Snapshot),
}

/// Start the worker. Returns immediately; events arrive on `sender`.
///
/// A bus that cannot be reached is not fatal — the HUD simply stays
/// dormant, exactly as it does when the publisher is not running.
pub fn spawn(sender: async_channel::Sender<BusEvent>) {
    std::thread::Builder::new()
        .name("myna-hud-dbus".into())
        .spawn(move || {
            if let Err(e) = run(sender) {
                eprintln!("myna-hud: D-Bus worker stopped: {e}");
            }
        })
        .expect("spawn D-Bus worker");
}

fn run(sender: async_channel::Sender<BusEvent>) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let dbus = DBusProxy::new(&connection)?;
    let name: BusName = BUS_NAME.try_into()?;

    let properties = PropertiesProxy::builder(&connection)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .build()?;

    // A publisher already running when we start must be reflected at once
    // (X8) — otherwise the HUD stays blank until the next transition.
    if dbus.name_has_owner(name.clone())? {
        if let Ok(snapshot) = read_snapshot(&properties) {
            let _ = sender.send_blocking(BusEvent::NameAppeared(snapshot));
        }
    }

    // Property pushes run on their own thread: both streams block, and the
    // owner stream must stay responsive so a crash clears the pill promptly.
    {
        let sender = sender.clone();
        let properties = properties.clone();
        std::thread::Builder::new()
            .name("myna-hud-props".into())
            .spawn(move || {
                let Ok(changes) = properties.receive_properties_changed() else {
                    return;
                };
                for _ in changes {
                    // Re-read the whole set rather than merging the
                    // signal's partial payload: the publisher pushes all
                    // four properties together, and a full read cannot
                    // drift from the peer's actual state.
                    match read_snapshot(&properties) {
                        Ok(snapshot) => {
                            if sender
                                .send_blocking(BusEvent::Properties(snapshot))
                                .is_err()
                            {
                                return; // the UI is gone
                            }
                        }
                        Err(_) => continue,
                    }
                }
            })
            .expect("spawn properties worker");
    }

    for change in dbus.receive_name_owner_changed()? {
        let Ok(args) = change.args() else { continue };
        if args.name().as_str() != BUS_NAME {
            continue;
        }
        let appeared = args.new_owner().as_ref().is_some();
        let event = if appeared {
            match read_snapshot(&properties) {
                Ok(snapshot) => BusEvent::NameAppeared(snapshot),
                Err(_) => BusEvent::NameAppeared(Snapshot::default()),
            }
        } else {
            BusEvent::NameVanished
        };
        if sender.send_blocking(event).is_err() {
            break; // the UI is gone
        }
    }
    Ok(())
}

/// Read all four properties in one round trip.
fn read_snapshot(properties: &PropertiesProxy<'_>) -> zbus::Result<Snapshot> {
    let interface: InterfaceName = INTERFACE.try_into()?;
    let all = properties.get_all(interface)?;
    Ok(snapshot_from(&all))
}

/// Build a [`Snapshot`] from a property map, tolerating anything missing or
/// oddly typed — an additive or partially-implemented publisher must never
/// take the HUD down (C8).
fn snapshot_from(map: &HashMap<String, OwnedValue>) -> Snapshot {
    Snapshot {
        state: string_of(map, "State"),
        error_message: string_of(map, "ErrorMessage"),
        audio_rms: double_of(map, "AudioRms"),
        audio_peak: double_of(map, "AudioPeak"),
    }
}

fn string_of(map: &HashMap<String, OwnedValue>, key: &str) -> String {
    map.get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn double_of(map: &HashMap<String, OwnedValue>, key: &str) -> f64 {
    map.get(key)
        .and_then(|v| f64::try_from(v.clone()).ok())
        .unwrap_or(0.0)
}
