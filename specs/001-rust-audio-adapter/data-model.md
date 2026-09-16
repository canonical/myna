# Data Model: Audio Adapter Library

**Feature**: `001-rust-audio-adapter` | **Date**: 2026-07-15

Entities are expressed as Rust-flavored types for precision; names are normative, exact field types may be refined during implementation.

## InputNode

An audio-producing node or source exposed by the audio server (physical microphone, monitor, virtual node). From FR-002 and the node-enumeration clarification.

| Field | Type | Notes |
|---|---|---|
| `id` | `NodeId` (opaque, backend-scoped) | Unique within a backend session; not persistent across server restarts |
| `name` | `String` | Stable machine name (e.g. `alsa_input.pci-0000_00_1f.3.analog-stereo`) |
| `description` | `String` | Human-readable label for UIs |
| `is_default` | `bool` | Whether this is the server's current default input |
| `supported_formats` | `Vec<AudioFormat>` | Rates/formats/channel layouts the node advertises |

**Identity/uniqueness**: `id` is unique per enumeration snapshot; `name` is the quasi-stable key consumers should persist (e.g., in Settings UI).

## AudioFormat

| Field | Type | Notes |
|---|---|---|
| `sample_rate` | `u32` (Hz) | e.g. 16_000 |
| `sample_format` | `SampleFormat` enum: `S16LE`, `F32LE`, … | Target default `S16LE` |
| `channels` | `u16` | Target default `1` (mono) |

Default target format: **16 kHz, mono, S16LE** (spec Assumptions).

## StreamConfig

Consumer-supplied settings for opening a stream (the spec's "Stream Configuration" entity; the library has streams, not sessions, per the stateless-primitives clarification).

| Field | Type | Default | Notes |
|---|---|---|---|
| `node` | `NodeSelector` enum: `Default` \| `ById(NodeId)` \| `ByName(String)` | `Default` | FR-002 |
| `target_format` | `AudioFormat` | 16 kHz / mono / S16LE | FR-004/FR-005 |
| `max_buffer_duration` | `Duration` | 10 s | FR-007; bounds the ring buffer |
| `preprocess` | `PreprocessConfig` | all disabled | FR-010/FR-011 |
| `backend` | `BackendSelector`: `Auto` \| `PipeWire` \| `Pulse` | `Auto` | testing/override hook |

### PreprocessConfig

| Field | Type | Default |
|---|---|---|
| `denoise` | `bool` | `false` |
| `vad` | `bool` | `false` |
| `deverb` | `bool` | `false` (reserved; stage deferred per research R6) |

**Validation rules**: `target_format.sample_rate` ∈ [8 kHz, 192 kHz]; `channels` ≥ 1; `max_buffer_duration` > 0; unsupported/unconvertible combinations fail `open_stream` with `Error::UnsupportedFormat` (FR-012).

## AudioStream

Handle to one open capture stream. One per input node (FR-003); `open_stream` on a node with an existing open stream returns the existing handle unchanged (idempotent "ensure open").

**State transitions** (stateless primitives — no session states, FR spec clarification):

```
(closed) --open_stream--> Open --close()/drop--> (closed)
                    Open --device lost--> Failed(DeviceLost)   [terminal; resources released]
                    Open --unconvertible format change--> Failed(UnsupportedFormat)
```

- `Open → Failed` delivers the error through the read path, closes the underlying server stream, and releases resources (FR-016).
- Close releases the audio source and clears buffers within 200 ms (FR-008, SC-004).
- Transparent renegotiation (FR-017) is **not** a state change — the stream stays `Open` and keeps delivering target-format frames.

## AudioFrame

A contiguous chunk of target-format audio with timing metadata (FR-013).

| Field | Type | Notes |
|---|---|---|
| `data` | `Bytes`/`Vec<u8>` (interleaved samples in target format) | Always target format |
| `format` | `AudioFormat` | Echo of the target format |
| `timestamp` | `Duration` (stream clock, monotonic from stream open) | Start of this frame |
| `duration` | `Duration` | Derivable from `data.len()`/format; carried for convenience |
| `seq` | `u64` | Monotonically increasing; no gaps (silence-fill keeps timeline continuous) |

**Invariants**: frames are contiguous and non-overlapping (`timestamp[n+1] = timestamp[n] + duration[n]`, FR-013/FR-018); no audible artifacts across frame boundaries (FR-015).

## StreamEvent

Out-of-band notifications interleaved with frames in the read results.

| Variant | Payload | Source requirement |
|---|---|---|
| `Overrun` | `{ dropped: Duration }` | FR-014 (oldest frames dropped) |
| `Underrun` | `{ filled: Duration }` | FR-018 (synthetic silence span) |
| `DeviceLost` | `{ node: NodeId }` — terminal, stream closed | FR-016 |
| `VoiceActivity` | `{ speaking: bool, at: Duration }` | US3 scenario 2 (only when VAD enabled) |

## Error

| Variant | Trigger |
|---|---|
| `NoDevice` | No microphone/node available at open (US1 scenario 3) |
| `PermissionDenied` | Audio server denies capture (FR-012) |
| `UnsupportedFormat` | Source format cannot be converted to target (FR-012/FR-017) |
| `DeviceLost` | Node disappeared while stream open (FR-016) |
| `Backend(String)` | Server connection/protocol failures |

## Relationships

```
AudioAdapter (library facade)
 ├── enumerate() ──> Vec<InputNode>
 ├── open_stream(StreamConfig) ──> AudioStream   (≤1 per InputNode, idempotent)
 │        AudioStream ──read()──> [AudioFrame | StreamEvent]  (bounded ring, ≤ max_buffer_duration)
 │        AudioStream ── PreprocessPipeline (0..n PreprocessStage: denoise → vad → [deverb])
 └── backends: PipeWire (primary) | Pulse (fallback)   [AudioBackend trait]
```

**Scale assumptions**: single desktop process, ≤ a handful of concurrently open streams, target-format data rate ≈ 32 KB/s (16 kHz × 2 bytes × mono) — 10 s buffer ≈ 320 KB per stream.
