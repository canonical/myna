//! serve — the `--serve-dbus` simulator: a fake `com.canonical.Myna.Dictation`
//! publisher (feature 004, T132; contract `dbus-interface.md`), the Rust
//! port of the former Python GPU lab's `dictation_service.py`.
//!
//! It claims the well-known name (never by force — a clean request, and it
//! bows out if `myna-desktop` already owns it), publishes
//! `State`/`ErrorMessage`/`AudioRms`/`AudioPeak` at [`PUBLISH_HZ`] from the
//! lab's controls, and answers `Start`/`Stop`/`Toggle`. This is what lets
//! the real hosted path — the extension consuming a live name — be exercised
//! without the Python daemon.
//!
//! The interface's *rules* are pure and tested elsewhere: the wire-state
//! mapping in [`crate::simulator`], the session dedup in
//! [`crate::session_control`]. This module is the zbus wiring that drives
//! them, plus the ~20 Hz publish loop.

use std::sync::{Arc, Mutex};

use zbus::interface;

use crate::session_control::Session;
use crate::simulator::{envelope_to_levels, PUBLISH_HZ};
use crate::states::wire;

use crate::dbus_consumer::{BUS_NAME, OBJECT_PATH};

/// The lab controls the simulator reads each publish tick — the same values
/// the lab UI edits, shared with the server.
#[derive(Clone, Debug)]
pub struct Controls {
    /// The wire `State` the lab selected (`recording`, `notice`, `idle`, …).
    pub state: String,
    /// The content-free reason for a `notice`/`error` state.
    pub reason: String,
    /// The smoothed envelope `[0, 1]`.
    pub envelope: f64,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            state: wire::IDLE.into(),
            reason: String::new(),
            envelope: 0.0,
        }
    }
}

/// The shared state between the lab UI, the publish loop and the method
/// handlers.
#[derive(Clone)]
pub struct Shared {
    controls: Arc<Mutex<Controls>>,
    session: Arc<Mutex<Session>>,
    publishing: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for Shared {
    fn default() -> Self {
        // Publishing defaults to true: Shared represents a live publisher.
        // The lab toggles it via set_publishing(false); the tests assume it.
        Self {
            controls: Arc::new(Mutex::new(Controls::default())),
            session: Arc::new(Mutex::new(Session::default())),
            publishing: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl Shared {
    /// Whether the publish loop should emit live state.
    pub fn is_publishing(&self) -> bool {
        self.publishing.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Gate publishing without releasing the bus name. When false, the
    /// snapshot forces idle so consumers see the HUD go quiet — the same
    /// observable effect as releasing the name, without the async
    /// name-release/re-claim race that calling `serve()` twice produces.
    pub fn set_publishing(&self, publishing: bool) {
        self.publishing
            .store(publishing, std::sync::atomic::Ordering::Relaxed);
    }

    /// Replace the live controls (called by the lab UI).
    ///
    /// The lab's state selector is its "what is showing" control, so it also
    /// drives the session: any non-`idle` choice means a live session,
    /// `idle` means stopped. Without this the lab would publish nothing
    /// until a session was separately started (via the bus `Start`/`Toggle`
    /// method or a hotkey) — surprising, since the lab has no other session
    /// control. External `Start`/`Stop`/`Toggle` callers drive the same
    /// flag, so a `gdbus … Toggle` still works too.
    pub fn set_controls(&self, controls: Controls) {
        let active = controls.state != wire::IDLE;
        *self.controls.lock().unwrap() = controls;
        self.session.lock().unwrap().set_active(active);
    }

    /// The `(State, ErrorMessage, rms, peak)` to publish right now, from the
    /// current controls and session flag. Public so the lab's slider→bus
    /// wiring can be pinned without a bus (tests/serve_levels.rs).
    pub fn snapshot(&self) -> (String, String, f64, f64) {
        let controls = self.controls.lock().unwrap().clone();
        let active = self.session.lock().unwrap().is_active();
        // When not publishing (the lab toggle is off), force idle so
        // consumers see the HUD go quiet — the observable effect of
        // "unpublish" without the name-release race.
        let active = active && self.is_publishing();
        let (state, reason) = if active {
            (controls.state.clone(), controls.reason.clone())
        } else {
            (wire::IDLE.to_string(), String::new())
        };
        let (rms, peak) = if active && state != wire::IDLE {
            envelope_to_levels(controls.envelope)
        } else {
            (0.0, 0.0)
        };
        (state, reason, rms, peak)
    }
}

/// The served `com.canonical.Myna.Dictation` object.
pub struct Dictation {
    shared: Shared,
    state: String,
    error_message: String,
    audio_rms: f64,
    audio_peak: f64,
}

impl Dictation {
    fn new(shared: Shared) -> Self {
        let (state, error_message, audio_rms, audio_peak) = shared.snapshot();
        Self {
            shared,
            state,
            error_message,
            audio_rms,
            audio_peak,
        }
    }
}

#[interface(name = "com.canonical.Myna.Dictation")]
impl Dictation {
    /// `Start`: begin a session (equivalent to a hotkey Press). The
    /// simulator never fails, so `ok` is always true (C7 shape preserved).
    async fn start(&mut self) -> (bool, String) {
        let outcome = self.shared.session.lock().unwrap().start();
        let (ok, error) = outcome.to_wire();
        (ok, error.to_string())
    }

    /// `Stop`: end the active session. No-op if idle.
    async fn stop(&mut self) {
        self.shared.session.lock().unwrap().stop();
    }

    /// `Toggle`: Start if idle, else Stop (dedup via
    /// [`Session`](crate::session_control::Session) — C6).
    async fn toggle(&mut self) {
        self.shared.session.lock().unwrap().toggle();
    }

    #[zbus(property)]
    fn state(&self) -> String {
        self.state.clone()
    }

    #[zbus(property)]
    fn error_message(&self) -> String {
        self.error_message.clone()
    }

    #[zbus(property)]
    fn audio_rms(&self) -> f64 {
        self.audio_rms
    }

    #[zbus(property)]
    fn audio_peak(&self) -> f64 {
        self.audio_peak
    }
}

/// Claim the name and start publishing. Returns once the server is running;
/// the publish loop lives on the connection's executor.
///
/// The name is requested *without* replacement: if `myna-desktop` already
/// owns `com.canonical.Myna.Dictation`, the simulator refuses rather than fighting the
/// real daemon (the lab is a stand-in, never an override).
pub async fn serve(shared: Shared) -> zbus::Result<zbus::Connection> {
    use zbus::fdo::RequestNameFlags;

    let object = Dictation::new(shared.clone());
    let connection = zbus::connection::Builder::session()?
        .serve_at(OBJECT_PATH, object)?
        .build()
        .await?;

    // Do NOT allow replacement and do NOT queue: either we get the name now
    // or the real daemon has it and we stand down.
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await;
    match reply {
        Ok(_) => {}
        Err(zbus::Error::NameTaken) => {
            return Err(zbus::Error::Failure(format!(
                "{BUS_NAME} is already owned (myna-desktop running?); the simulator stands down"
            )));
        }
        Err(e) => return Err(e),
    }

    spawn_publish_loop(connection.clone(), shared);
    Ok(connection)
}

/// Publish the current snapshot at [`PUBLISH_HZ`], emitting a
/// `PropertiesChanged` only for the properties that actually changed — the
/// one push channel a confined client is allowed to receive (the interface
/// has no custom signals).
fn spawn_publish_loop(connection: zbus::Connection, shared: Shared) {
    let period = std::time::Duration::from_secs_f64(1.0 / PUBLISH_HZ);
    let executor = connection.executor().clone();
    executor
        .spawn(
            async move {
                loop {
                    // zbus already pulls in async-io; use its timer rather than
                    // starting a second runtime.
                    async_io::Timer::after(period).await;
                    if let Err(e) = publish_once(&connection, &shared).await {
                        eprintln!("myna-hud --serve-dbus: publish failed: {e}");
                    }
                }
            },
            "myna-dictation-publish",
        )
        .detach();
}

async fn publish_once(connection: &zbus::Connection, shared: &Shared) -> zbus::Result<()> {
    let iface_ref = connection
        .object_server()
        .interface::<_, Dictation>(OBJECT_PATH)
        .await?;
    let (state, error_message, audio_rms, audio_peak) = shared.snapshot();

    let mut iface = iface_ref.get_mut().await;
    let emitter = iface_ref.signal_emitter();

    // The publisher pushes the whole property set every tick; emit a change
    // only where the value moved, so an unchanged State does not restart the
    // consumer's notice timers (mirrors the consumer's own dedup, C2).
    if iface.state != state {
        iface.state = state;
        iface.state_changed(emitter).await?;
    }
    if iface.error_message != error_message {
        iface.error_message = error_message;
        iface.error_message_changed(emitter).await?;
    }
    // Levels are pushed every tick, unconditionally — arrival time is part
    // of the stale-decay contract (R16a), so identical values still refresh.
    iface.audio_rms = audio_rms;
    iface.audio_peak = audio_peak;
    iface.audio_rms_changed(emitter).await?;
    iface.audio_peak_changed(emitter).await?;
    Ok(())
}
