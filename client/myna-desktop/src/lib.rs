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
//! hermetic-testable (no D-Bus / IBus / portal / display):
//! - [`inject::Injector`] — text injection ([`inject::ibus::IbusInjector`] /
//!   [`inject::mock::MockInjector`]);
//! - `shortcut` — activation, reusing `myna_orchestrator::Trigger`
//!   ([`shortcut::portal::GlobalShortcutTrigger`]);
//! - [`indicator::Indicator`] — the activity surface
//!   ([`indicator::notify::NotifyIndicator`] for headless, and the
//!   myna-shell overlay for GNOME; [`indicator::mock::MockIndicator`] for
//!   tests). The former GTK overlay was removed in T150.
//!
//! [`chime::ChimingIndicator`] is an optional `Indicator` decorator playing
//! start/stop/error chimes (STT UX spec's state-cue table) alongside
//! whichever indicator above is in use; wired in when the `chimes-enabled`
//! setting is on.
//!
//! Real IBus/portal/GTK behavior lives behind env-gated integration suites
//! (`MYNA_IBUS_TESTS` / `MYNA_PORTAL_TESTS`); the hermetic
//! suite drives the controller through the mocks.

pub mod backend;
pub mod chime;
pub mod controller;
pub mod dbus;
pub mod indicator;
pub mod inject;
pub mod live;
pub mod shortcut;

pub use chime::{Chime, ChimePlayer, ChimingIndicator, PipeWireChimePlayer};
pub use controller::{
    auto_stop_due, event_to_indicator, input_quality, AutoStop, ChannelSink, DesktopController,
    DesktopControllerBuilder, DictationState, InputQuality, Session, SessionFactory, SessionRun,
};
pub use indicator::{Indicator, IndicatorState};
pub use inject::{FocusEvent, InjectError, InjectionTarget, Injector};
pub use live::Live;
pub use shortcut::{Trigger, TriggerEdge};
