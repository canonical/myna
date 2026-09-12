# Phase 1 Data Model: Native PipeWire Capture Backend

**Feature**: 002-native-pipewire-backend · **Date**: 2026-07-15

Entities are Rust types in `rust/myna-audio`. Reused types are named but not
redefined (they do not change). New types are specified with fields, invariants,
and lifecycle.

## Reused (unchanged) — for reference only

- **`AudioFormat`** (`myna-core`): `{ sample_rate_hz, channels, sample_width_bytes }`,
  default 16 kHz mono S16LE. The negotiated capture format.
- **`PcmChunk`** (`myna-core`): `{ data: Bytes, format }` — whole-frame ~100 ms
  chunks emitted by the adapter.
- **`CaptureSpec`** (`myna-audio::backend`): `{ format, target: Option<String>,
  channels: Option<Vec<u8>>, stop: StopHandle }` — the capture request the new
  backend consumes. **No shape change**; the native backend simply honors
  `channels` (which the subprocess backend rejected) and resolves `target` via
  the registry.
- **`Producer`** (`myna-audio::backend`): sync non-blocking `push(Bytes) -> bool`
  + `finish(Option<CaptureError>)`. The native backend's sole output path.
- **`CaptureError`** (`myna-core`): `DeviceUnavailable(String)` /
  `UnsupportedFormat(AudioFormat)` / `Backend(String)`. Reused verbatim.
- **`AudioStats`** (`myna-audio::stats`): levels + captured/dropped durations.
  Unchanged; populated by the adapter core, not the backend.

## New: `PipeWireBackend`

The native capture backend — a `CaptureBackend` implementor.

- **Fields (private)**: configuration only until `start`; the live loop/thread
  handle is created in `start`. No public fields.
- **Construction**: `PipeWireBackend::new()` (default). A test/config seam MAY
  exist for pointing at a specific remote/loop, mirroring how `PwRecordBackend`
  had a command seam — kept minimal.
- **Behavior**: implements `CaptureBackend::start(self, spec, producer)`:
  - Validates `spec.format.sample_width_bytes == 2` → else
    `Err(UnsupportedFormat)`.
  - Spawns the PipeWire loop thread; connects an input `Stream` in `spec.format`,
    targeting `spec.target` (by `node.name`) or default; applies `spec.channels`
    (pick/downmix) when `Some`.
  - Returns `Ok(())` promptly (open failure → `Err(DeviceUnavailable | Backend)`).
  - Runtime: each `process` callback → `producer.push(bytes)`; stop/abort →
    `finish(None)`; error → `finish(Some(..))`.
- **Invariants**: produces EXACTLY `spec.format`; never persists; `push` never
  blocks; exactly one terminal outcome (`finish` once); stop observed ≤250 ms.
- **State/lifecycle**: `Configured → Capturing → (Stopped | Aborted | Faulted) →
  Finished`. Terminal state maps to `finish(None)` (stopped/aborted) or
  `finish(Some)` (faulted). Matches the existing `CaptureBackend` contract so the
  adapter core / ring / stream semantics are unchanged.

## New: `InputDevice`

A discoverable input device descriptor (spec "Input device descriptor").

- **Fields**:
  - `node_name: String` — stable PipeWire `node.name`; the selector used as
    `CaptureSpec.target`. **Identity**: unique per device; stable across graph
    changes/reconnect.
  - `label: String` — human-readable (`node.description`); for display only,
    never a selector.
- **Validation**: `node_name` non-empty; a device with no `node.name` is skipped
  (can't be selected stably).
- **Source**: derived from registry `global` objects with
  `media.class = Audio/Source` (monitors of sinks excluded by default).

## New: `DeviceChange`

A live enumeration event (spec "Device change notification", FR-008a).

- **Variants**:
  - `Added(InputDevice)` — a new input device appeared (carries name + label).
  - `Removed { node_name: String }` — a device disappeared (carries stable id).
- **Ordering/lifecycle**: emitted in registry-event order while an observer is
  active; an `Added` for a device already present is not re-emitted on subscribe
  (subscribers get the current snapshot first, then deltas).

## New: `InputDevices` (enumerator handle)

The live enumeration capability.

- **Construction**: `InputDevices::new()` → starts (or attaches to) a registry
  listener on a PipeWire loop thread; `Result<Self, CaptureError>` (a failure to
  connect to PipeWire is `DeviceUnavailable`).
- **Accessors**:
  - `list(&self) -> Vec<InputDevice>` — current snapshot (empty list, not error,
    when none present — US4 scenario 2 / FR-008).
  - `watch(&self) -> watch::Receiver<Vec<InputDevice>>` — latest full list,
    conflating; updates as devices appear/disappear (primary UI surface).
  - `changes(&self) -> broadcast::Receiver<DeviceChange>` — add/remove deltas for
    observers that want events rather than the whole list (secondary; include
    only if a consumer needs deltas — otherwise `watch` alone satisfies FR-008a).
- **Invariants**: read-only; carries no audio; the watch/broadcast reflect
  registry state within event latency. Dropping the handle stops the listener and
  releases the loop thread.

## Relationships

```text
CaptureSpec.target ──(node.name)──▶ InputDevice.node_name        (selection, FR-004)
CaptureSpec.channels ─────────────▶ PipeWireBackend channel pick/downmix (FR-006)
PipeWireBackend.start ── push ────▶ Producer ─▶ ring ─▶ CaptureStream (unchanged core)
InputDevices ── watch/changes ────▶ DeviceChange / Vec<InputDevice>  (FR-008/008a)
```

`InputDevices` and `PipeWireBackend` are independent public entry points that
happen to share the PipeWire connection model; a consumer can enumerate without
capturing and capture without enumerating (the session controller passes a chosen
`node.name` straight into `CaptureSpec.target`).
