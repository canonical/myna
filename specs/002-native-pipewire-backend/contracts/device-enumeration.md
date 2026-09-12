# Contract: live input-device enumeration

**Feature**: 002-native-pipewire-backend · **Crate**: `rust/myna-audio`

A new, small public API for discovering input devices and observing changes
live. Consumed by the session controller / a future settings chooser; produces
the stable `node.name` that feeds `CaptureSpec.target`.

## Interface (new)

```rust
/// A discoverable input device.
pub struct InputDevice {
    pub node_name: String,  // stable selector (PipeWire node.name)
    pub label: String,      // human-readable (node.description)
}

/// A live enumeration event.
pub enum DeviceChange {
    Added(InputDevice),
    Removed { node_name: String },
}

/// Live input-device enumerator. Dropping it stops the listener.
pub struct InputDevices { /* … */ }

impl InputDevices {
    pub fn new() -> Result<Self, CaptureError>;
    pub fn list(&self) -> Vec<InputDevice>;
    pub fn watch(&self) -> tokio::sync::watch::Receiver<Vec<InputDevice>>;
    pub fn changes(&self) -> tokio::sync::broadcast::Receiver<DeviceChange>; // optional (see note)
}
```

> **Note (design decision to confirm in implementation):** `watch<Vec<InputDevice>>`
> alone satisfies FR-008a (a chooser reads the latest list, which updates live).
> `changes()` is included only if an observer needs deltas rather than the whole
> list; if no consumer needs deltas at implementation time, omit it (additive
> later — not a breaking change). Do not build both without a consumer.

## Guarantees (each row → at least one test)

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| E1 | a known set of virtual input nodes | `list()` | every expected device returned with stable `node_name` + `label` | FR-008, SC-005, US4-1 |
| E2 | no input devices present | `list()` | empty `Vec`, not an error | FR-008, US4-2 |
| E3 | an active `watch()`/`changes()` observer | a device appears | observer sees the new device (name + label) without re-requesting | FR-008a, US4-3 |
| E4 | an active observer | a device disappears | observer sees the removal by stable `node_name` | FR-008a, US4-3 |
| E5 | PipeWire not reachable | `new()` | `Err(CaptureError::DeviceUnavailable)` | FR-010 (fault surfacing) |
| E6 | any call | — | read-only: no audio captured, nothing persisted | FR-013, Principle V |
| E7 | a name from `list()` used as `CaptureSpec.target` | capture | selects that device (ties enumeration to selection) | FR-004 + FR-008 |

## Test homes

- **Integration (env-gated, virtual-audio VM + hardware)**: E1–E5, E7 against a
  PipeWire graph where nodes are added/removed via `pw-loopback` (create/kill) to
  drive appearance/disappearance. `tests/pipewire_hw.rs`.
- **Hermetic**: the `InputDevice`/`DeviceChange` data shapes and any pure mapping
  logic (registry-props → `InputDevice`, skip-no-name, exclude monitors) are unit
  tested without a server.

## Non-goals

- No device *configuration* (volume, profile) — read/observe only.
- No output-device enumeration (input sources only).
- No persistence of the device list.
