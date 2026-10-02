//! The activation boundary, reusing the orchestrator's `Trigger` trait
//! (`myna_orchestrator::trigger`) unchanged. A desktop custom shortcut pokes
//! the daemon ([`control::ControlTrigger`]); `ScriptedTrigger` (orchestrator)
//! is the hermetic fixture.

pub mod control;
pub mod dbus;
pub mod retry;

pub use myna_orchestrator::{Trigger, TriggerEdge};
