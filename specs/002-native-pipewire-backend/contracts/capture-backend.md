# Contract: `PipeWireBackend` (capture-backend seam)

**Feature**: 002-native-pipewire-backend · **Crate**: `rust/myna-audio`

The native backend implements the **existing, unchanged** `CaptureBackend` trait.
This contract restates the guarantees the native implementation must satisfy so
they can be encoded as executable tests (TDD, Principle I) before the code.

## Interface (unchanged trait)

```rust
pub trait CaptureBackend: Send {
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer)
        -> Result<(), CaptureError>;
}
```

`PipeWireBackend: CaptureBackend`, constructed via `PipeWireBackend::new()`, used
through `CaptureSource::builder(fmt).backend(Box::new(PipeWireBackend::new()))`.

## Guarantees (each row → at least one test)

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| C1 | a working default source, no `target` | `capture()` (press) then drain after "ready" | ring fills from press; drained chunks are exactly `spec.format`; a known utterance transcribes correctly | FR-001, FR-003, FR-009, SC-001 |
| C2 | device native format ≠ negotiated (rate/channels) | capture runs | consumer receives EXACTLY the negotiated format (graph-converted) | FR-003, US1-2 |
| C3 | a valid `target` node.name | capture | audio comes from that node, not the default | FR-004, US2-1 |
| C4 | a `target` that is absent at connect | `capture()` | stream yields exactly one `Err(DeviceUnavailable(target))`, then ends | FR-004, FR-010, US2-3 |
| C5 | a stable `target`, then a graph change reassigns volatile ids | capture after the change | same node still selected | FR-004, SC-003 |
| C6 | multi-channel device, `channels = Some(idx…)` | capture | only those channels captured, downmixed to negotiated layout | FR-006, SC-004, US3-1 |
| C7 | `channels` the device can't satisfy | `capture()` | exactly one `Err` (Backend/Unsupported), no mis-capture | FR-007, US3-2 |
| C8 | active capture | `stop()` (graceful) | captured audio drains, stream ends with no `Err` | FR-011, SC-007 |
| C9 | active capture | drop the stream (abort) | capture stops, ring discarded, resources released | FR-011 |
| C10 | active capture | device disappears mid-capture | exactly one `Err` (descriptive), then end — never empty-clean | FR-010, SC-007 |
| C11 | `stop()`/abort at any time | — | honored within 250 ms | FR-012, SC-009 |
| C12 | `sample_width_bytes != 2` | `capture()` | one `Err(UnsupportedFormat)` | FR-003 (encoding assumption) |
| C13 | any healthy session | drain keeps up | `AudioStats::dropped == 0`; stats update at capture time | FR-014, SC-006 |
| C14 | any session | capture path runs | no external process spawned; nothing written to disk | FR-001, FR-013, SC-002 |

## Test homes

- **Hermetic (no audio server)**: the seam-level guarantees that don't require
  real PipeWire (fault-is-one-Err shape, stop drains, abort discards, unsupported
  format/channels rejection) are already covered for *any* backend by the
  `ScriptedBackend` suite in `tests/adapter.rs` and mirrored where the native
  backend can be driven with a scripted/loopback stand-in. These stay green
  offline.
- **Integration (virtual-audio VM + hardware, env-gated)**: C1–C6, C10, C13, C14
  run against a real PipeWire graph (`pw-loopback`/null-sink + known signal) in
  `tests/pipewire_hw.rs`, gated on `MYNA_PIPEWIRE_TESTS=1`. Identical code on the
  VM and on hardware (Principle II).

## Non-goals (explicitly not in this contract)

- No in-crate DSP (noise suppression etc. stay in the PipeWire graph).
- No VAD (push-to-talk; the hotkey is the trigger).
- No change to `CaptureSpec`, `Producer`, `AudioStats`, `CaptureError`, or the
  `AudioSource`/`CaptureStream` consumer types.
