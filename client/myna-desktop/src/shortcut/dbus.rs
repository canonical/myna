//! `DbusTrigger` — a [`Trigger`] backend fed by the `org.myna.Dictation`
//! `Start`/`Stop`/`Toggle` D-Bus methods (feature 004, contract publisher.md
//! P9–P12), sibling to `ControlTrigger` with the same alternation/dedup so the
//! panel button is equivalent to the hotkey (C6). Implementation lands with
//! its hermetic suite (US4).

/// Feeds `TriggerEdge`s into the orchestrator's `Trigger` seam from D-Bus
/// method calls. `Start` returns `(ok, reason)` with a content-free reason (C7).
pub struct DbusTrigger;
