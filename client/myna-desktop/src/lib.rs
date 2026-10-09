//! `myna-desktop` — the dictation last-mile (plan T21/T22, feature
//! 003-desktop-injection).
//!
//! A **desktop session controller** ([`DesktopController`]) that a global
//! shortcut activates for push-to-talk, driving the *existing* client
//! capture→FSM→transcript path (feature 002 native capture +
//! `myna-orchestrator`), and a **text-injection backend** ([`Injector`]) that
//! inserts committed transcripts into the application focused when the session
//! started. A small **activity indicator** ([`Indicator`]) shows recording /
//! transcribing / finalizing / error.
//!
//! Three boundary seams, each with a mock so the controller is fully
//! hermetic-testable (no D-Bus / IBus / display):
//! - [`inject::Injector`] — text injection ([`inject::ibus::IbusInjector`] /
//!   [`inject::mock::MockInjector`]);
//! - `shortcut` — activation, reusing `myna_orchestrator::Trigger`
//!   ([`shortcut::control::ControlTrigger`]);
//! - [`indicator::Indicator`] — the activity surface
//!   ([`indicator::notify::NotifyIndicator`] for headless, and the
//!   myna-shell overlay for GNOME; [`indicator::mock::MockIndicator`] for
//!   tests).
//!
//! Real IBus/GTK behavior lives behind env-gated integration suites
//! (`MYNA_IBUS_TESTS` / `MYNA_DBUS_TESTS`); the hermetic suite drives the
//! controller through the mocks.

pub mod controller;
pub mod dbus;
pub mod indicator;
pub mod inject;
pub mod live;
pub mod platform;
pub mod shortcut;
pub mod sound;

pub use controller::{
    auto_stop_due, event_to_indicator, input_quality, AutoStop, ChannelSink, Delivery,
    DesktopController, DesktopControllerBuilder, DictationState, InputQuality, Session,
    SessionFactory, SessionRun,
};
pub use indicator::{Indicator, IndicatorState};
pub use inject::{FocusEvent, InjectError, Injector, Target};
pub use live::Live;
pub use shortcut::{Trigger, TriggerEdge};
