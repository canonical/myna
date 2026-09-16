# Contract: `myna-audio-adapter` Public API

**Feature**: `001-rust-audio-adapter` | **Type**: Rust library API (the crate's external interface)

This is the contract the Speech Controller (and tests) program against. Signatures are normative in shape; exact generics/lifetimes may be refined without changing observable behavior. Types referenced here are defined in [data-model.md](../data-model.md). The "Known consumer" section below pins the subset of this surface the Speech Controller actually uses — API changes must keep that section and its consumer-scenario test in sync (FR-020).

## Entry points

```rust
/// Enumerate audio-producing input nodes with metadata (FR-002, clarification on enumeration).
/// Errors: Backend (server unreachable).
pub fn enumerate_nodes() -> Result<Vec<InputNode>, Error>;

/// Ensure a capture stream is open on the selected node and return its handle (FR-003).
/// Idempotent: if the node already has an open stream, returns the existing handle; the
/// provided config MUST NOT alter the already-open stream.
/// Errors: NoDevice, PermissionDenied, UnsupportedFormat, Backend (FR-012).
/// Postcondition: first frame is readable within 100 ms on reference hardware (SC-001).
pub fn open_stream(config: &StreamConfig) -> Result<AudioStream, Error>;
```

## `AudioStream`

```rust
impl AudioStream {
    /// Non-blocking: drain whatever is buffered (possibly empty).
    /// Items appear in timeline order; StreamEvents are interleaved at the position
    /// in the timeline where they occurred.
    pub fn read(&mut self) -> Result<Vec<StreamItem>, Error>;

    /// Blocking with bound: wait until at least one item is available or the timeout elapses.
    pub fn read_timeout(&mut self, timeout: Duration) -> Result<Vec<StreamItem>, Error>;

    /// The node this stream captures from.
    pub fn node(&self) -> &InputNode;

    /// The configured target format; every AudioFrame matches it (FR-004/FR-005).
    pub fn target_format(&self) -> AudioFormat;

    /// Close: stop delivery, release the audio source, clear buffers (FR-008).
    /// Completes within 200 ms (SC-004). Also invoked by Drop.
    pub fn close(self);
}

pub enum StreamItem {
    Frame(AudioFrame),
    Event(StreamEvent),
}
```

### Optional async adapter (feature = "async")

```rust
impl AudioStream {
    /// futures::Stream<Item = Result<StreamItem, Error>> over the same core.
    pub fn into_stream(self) -> impl futures_core::Stream<Item = Result<StreamItem, Error>>;
}
```

## Known consumer: Speech Controller (dictation client)

The primary consumer is the Speech Controller defined in `docs/architecture/UD129 - Ubuntu Desktop STT Integration.md`. This section pins the exact API surface it uses (FR-020); the consumer-scenario test (`tests/consumer_scenario.rs`) exercises this surface in this call pattern and MUST break if any of it changes incompatibly.

**Consumer surface** (everything the Speech Controller touches — nothing else in the crate is consumer-facing):

| Speech Controller responsibility (UD129) | API surface used |
|---|---|
| Microphone selection in Settings UI | `enumerate_nodes()` → `InputNode { id, name, description, is_default, supported_formats }`; persists `name`, re-resolves at session start |
| Session start (hotkey press → `Starting`) | `open_stream(&StreamConfig { node, target_format, preprocess, .. })`; first frame ≤ 100 ms (G9) covers the "capture within 100 ms" UD129 target |
| Stream audio frames to Inference Snap (`Recording`/`Transcribing`) | loop: `read_timeout(d)` → forward `StreamItem::Frame(AudioFrame)` payloads; or `into_stream()` (feature `async`) in an async orchestrator |
| Utterance chunking / finalization hints | `StreamItem::Event(VoiceActivity { speaking, at })` (feature `vad`) |
| Diagnostics without raw audio | `Overrun { dropped }` / `Underrun { filled }` events; timing metadata on frames |
| Error states shown to user (no mic, permission denied) | `Error::NoDevice`, `Error::PermissionDenied` from `open_stream` |
| Mic unavailable mid-session → stop and notify (UD129 failure handling) | `StreamItem::Event(DeviceLost)` then stream is closed; controller ends the session |
| Session end / cancellation (hotkey release → buffers discarded) | `close()`; ≤ 200 ms, buffers cleared (G8) covers UD129 "discard in-memory audio buffer" |

**Canonical consumer call sequence** (the shape `tests/consumer_scenario.rs` asserts):

```rust
// settings time
let nodes = enumerate_nodes()?;                       // populate device picker
// hotkey pressed
let mut stream = open_stream(&config)?;               // Starting → Recording
while session_active {                                 // Recording/Transcribing
    for item in stream.read_timeout(FRAME_WAIT)? {
        match item {
            StreamItem::Frame(f) => inference.send(f.data),      // stream to Inference Snap
            StreamItem::Event(VoiceActivity { speaking: false, .. }) => chunk_utterance(),
            StreamItem::Event(DeviceLost { .. }) => { end_session_with_error(); }
            StreamItem::Event(_) => log_diagnostics(),           // Overrun/Underrun
        }
    }
}
// hotkey released (or cancelled)
stream.close();                                        // Finalizing → Idle; buffers cleared
```

**Explicitly not consumer surface**: `BackendSelector` override (test hook), `MockBackend`, the `AudioBackend`/`PreprocessStage` traits, ring-buffer internals. Changes there are invisible to the Speech Controller by design.

## Behavioral guarantees (contract tests assert these)

| # | Guarantee | Source |
|---|---|---|
| G1 | Every `AudioFrame` matches `target_format` exactly, regardless of source format | FR-004/FR-005, US2-1 |
| G2 | Frames are contiguous and non-overlapping: `timestamp[n+1] == timestamp[n] + duration[n]`, `seq` has no gaps | FR-013 |
| G3 | Buffer full ⇒ oldest frames dropped, exactly one `Overrun{dropped}` event per loss span, smoothed splice (no clicks/clipping) | FR-014/FR-015 |
| G4 | Server underrun ⇒ silence fill keeps timeline continuous + one `Underrun{filled}` event; fill boundaries smoothed (no clipping artifacts) | FR-018/FR-015 |
| G5 | Node lost while open ⇒ `DeviceLost` event delivered, stream closed, resources released; the library never retargets on its own | FR-016 |
| G6 | Mid-stream source format change ⇒ transparent renegotiation, uninterrupted target-format delivery; `UnsupportedFormat` error only if unconvertible | FR-017, US2-3 |
| G7 | `open_stream` on an already-open node is a no-op returning the existing stream | FR-003 |
| G8 | After `close()`, no frames are delivered; buffers cleared; source released within 200 ms | FR-008, SC-004 |
| G9 | First frame available ≤ 100 ms after `open_stream` (reference hardware) | SC-001 |
| G10 | End-to-end latency ≤ 100 ms behind real time under normal load | SC-003 |
| G11 | With VAD enabled, `VoiceActivity{speaking:false}` fires when speech stops | US3-2 |
| G12 | With preprocessing disabled, converted frames pass through with no added latency stages | US3-3 |
| G13 | No audio is written to disk by the library; buffers bounded by `max_buffer_duration` | FR-007 |
| G14 | No network access required for any code path | SC-006 |
| G15 | The Speech Controller consumer surface (section above) works end-to-end in the canonical call sequence: enumerate → open → read loop with events → close | FR-020 |

## Feature flags

| Feature | Default | Adds |
|---|---|---|
| `pipewire` | yes | Native PipeWire backend (primary) |
| `pulse` | yes | PulseAudio fallback backend |
| `vad` | no | Silero VAD preprocessing stage + `VoiceActivity` events |
| `denoise` | no | RNNoise (`nnnoiseless`) preprocessing stage |
| `async` | no | `futures::Stream` adapter |
| `test-util` | no | `MockBackend` for consumer/test use — not part of the consumer surface |

## Stability

Pre-1.0 (`0.x`): breaking API changes allowed with minor-version bumps, tracked in CHANGELOG. The `StreamItem`/`StreamEvent`/`Error` enums are `#[non_exhaustive]` so variants can be added without breakage.
