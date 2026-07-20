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
//!   ([`indicator::notify::NotifyIndicator`], the `ui-gtk`
//!   [`indicator::gtk::GtkIndicator`], [`indicator::mock::MockIndicator`]).
//!
//! Real IBus/portal/GTK behavior lives behind env-gated integration suites
//! (`MYNA_IBUS_TESTS` / `MYNA_PORTAL_TESTS` / a display gate); the hermetic
//! suite drives the controller through the mocks.

pub mod controller;
pub mod indicator;
pub mod inject;
pub mod shortcut;

pub use controller::{
    event_to_indicator, ChannelSink, DesktopController, DesktopControllerBuilder, DictationState,
    SessionFactory, SessionRun,
};
pub use indicator::{Indicator, IndicatorState};
pub use inject::{FocusEvent, InjectError, InjectionTarget, Injector};
pub use shortcut::{Trigger, TriggerEdge};
