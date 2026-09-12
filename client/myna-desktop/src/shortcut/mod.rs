//! The activation boundary — reuses the orchestrator's `Trigger` trait
//! (`myna_orchestrator::trigger`) unchanged. The desktop's production activation
//! is a hold-to-talk global shortcut via the `org.freedesktop.portal.
//! GlobalShortcuts` portal ([`portal::GlobalShortcutTrigger`], branch 003c/US2);
//! for the MVP the orchestrator's `StdinTrigger` stands in. `ScriptedTrigger`
//! (orchestrator) is the hermetic fixture. See
//! `specs/003-desktop-injection/contracts/trigger.md`.

pub mod control;
pub mod dbus;
pub mod portal;
pub mod retry;

pub use myna_orchestrator::{Trigger, TriggerEdge};
