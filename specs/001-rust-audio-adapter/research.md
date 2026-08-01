# Research: Audio Adapter Library

**Feature**: `001-rust-audio-adapter` | **Date**: 2026-07-15

All Technical Context unknowns from plan.md are resolved below.

## R1. Audio backend strategy (PipeWire vs PulseAudio vs portability layers)

- **Decision**: Define an internal `AudioBackend` trait with two implementations: a **native PipeWire backend** (via the `pipewire` crate, the official pipewire-rs bindings) as the primary path, and a **PulseAudio backend** (via `libpulse-binding` + `libpulse-simple-binding`) as the fallback for systems running a real PulseAudio daemon. Backend selection is automatic at runtime (probe PipeWire first, fall back to PulseAudio), with an override in `StreamConfig` for testing.
- **Rationale**:
  - FR-001 requires both PipeWire- and PulseAudio-compatible servers. Ubuntu 22.10+ ships PipeWire (with `pipewire-pulse` emulating PulseAudio), but PulseAudio-only systems still exist in the support window.
  - FR-002/the node-enumeration clarification require enumerating *arbitrary audio-producing nodes* (not just microphones) with format metadata — only the native PipeWire registry exposes this fully; the PulseAudio backend enumerates sources (including monitor sources) which covers the same user need on PA systems.
  - FR-009 prefers native/session-manager routing (WirePlumber) — only available through the native PipeWire API.
- **Alternatives considered**:
  - **`libpulse` only** (works against pipewire-pulse too): simplest, but cannot enumerate PipeWire nodes generically, cannot leverage WirePlumber routing, and adds a compat layer on the primary platform. Rejected as primary; kept as fallback.
  - **`cpal`**: cross-platform, but its Linux story is ALSA-first, node enumeration is weak, and it hides format-renegotiation events we must surface (FR-017). Rejected.
  - **GStreamer**: capable but a heavyweight dependency tree for a focused capture library. Rejected.

## R2. Resampling and format conversion

- **Decision**: Prefer **server-side negotiation**: request the target format (default 16 kHz / mono / S16LE) directly on the capture stream so PipeWire's `audioconvert`/`audioresample` (or PulseAudio's stream converter) delivers target-format frames natively. When the server cannot honor the target (or changes the source format mid-stream, FR-017), fall back to an **in-process conversion stage**: `rubato` (sinc-interpolation resampler, pure Rust) for sample-rate conversion plus a small hand-rolled sample-format/channel-mixdown step.
- **Rationale**: Matches the feature input's "native (session manager provided when possible) functionality" requirement (FR-009) and keeps the hot path zero-copy in the common case; `rubato` is the de-facto Rust resampler, real-time-safe (no allocation in `process`), and licence-compatible (MIT).
- **Alternatives considered**: `libsoxr`/`speexdsp` bindings (C dependencies, packaging burden), `dasp` (no polyphase/sinc resampler of comparable quality), always-in-process conversion (wastes the server's optimized path and contradicts FR-009).

## R3. Buffering, overrun, and underrun handling

- **Decision**: One bounded **SPSC ring buffer** per open stream (via the `ringbuf` crate), sized from `max_buffer_duration` (default 10 s, FR-007). The real-time capture callback is the producer; the consumer pulls via the read API. On overflow the producer drops the **oldest** frames, records the dropped span, and emits an `Overrun` event (FR-014). On server underrun the library inserts **silence** for the missing span and emits an `Underrun` event (FR-018). All splice points (drop boundaries, silence-fill boundaries) get a short raised-cosine fade (~5 ms) to prevent clicks/clipping (FR-015).
- **Rationale**: SPSC lock-free ring keeps the capture callback allocation- and lock-free (a hard real-time-audio requirement); drop-oldest keeps latency bounded for live dictation; explicit events keep the delivered timeline honest and continuous per the clarifications.
- **Alternatives considered**: `crossbeam` channels (allocation + unbounded by default), blocking the callback (audio-server stalls, unacceptable), abrupt splices without smoothing (violates FR-015).

## R4. Delivery model (pull API, events, threading)

- **Decision**: **Synchronous pull-based core**: the backend runs its own event-loop thread (PipeWire `MainLoop` / PulseAudio threaded mainloop); the consumer calls `AudioStream::read()` (non-blocking, returns whatever is buffered) or `read_timeout()` (bounded wait). Out-of-band notifications (overrun, underrun, device-lost, voice-activity) are delivered as `StreamEvent`s interleaved in the read results. An optional `async` feature exposes a `futures::Stream` adapter over the same core.
- **Rationale**: The clarifications fixed a pull model ("consumer pulls at irregular intervals") with stateless open/close primitives; a sync core with an async adapter serves both the Speech Controller (likely async) and tests without forcing a runtime dependency on all consumers.
- **Alternatives considered**: callback-based push API (inverts control, complicates consumer lifecycle and violates the stateless-primitives decision), mandatory tokio dependency (unnecessary coupling for a leaf library).

## R5. Voice activity detection

- **Decision**: Feature-gated `vad` stage using **Silero VAD** through the `voice_activity_detector` crate (onnxruntime backend). Emits `VoiceActivity { speaking: bool }` stream events on transitions (US3, FR-010).
- **Rationale**: Silero is the current accuracy standard for lightweight offline VAD, runs comfortably in real time on CPU at 16 kHz, and the ONNX model is redistributable. Feature-gating keeps the onnxruntime dependency out of consumers that disable preprocessing.
- **Alternatives considered**: `webrtc-vad` (much lighter but markedly worse accuracy, energy-based failure modes in noise), energy-threshold VAD (too crude for utterance chunking), custom DNN (out of scope).

## R6. Noise suppression and dereverberation

- **Decision**: Feature-gated `denoise` stage using **`nnnoiseless`** (pure-Rust RNNoise port, 48 kHz internal rate handled by the conversion stage). Dereverberation (FR-011, MAY) is **deferred**: the preprocessing pipeline is a trait-based stage chain (`PreprocessStage`), so a deverb stage can be added later without API changes; no mature pure-Rust dereverb implementation exists today.
- **Rationale**: `nnnoiseless` is proven, allocation-free in steady state, and pure Rust (no C build). Deferring deverb honors the MAY requirement without blocking the MVP.
- **Alternatives considered**: RNNoise C bindings (build complexity for no quality gain), DeepFilterNet (better quality but heavy model + LADSPA/ONNX integration cost — candidate for a later stage behind the same trait), doing nothing (loses FR-010 SHOULD).

## R7. Testing strategy against a real audio server

- **Decision**: Three layers:
  1. **Unit tests** with a `MockBackend` implementing `AudioBackend`, feeding golden WAV fixtures — covers conversion, ring buffer, overrun/underrun/smoothing, VAD gating, error mapping.
  2. **Integration tests** against a real PipeWire instance using a **null-sink virtual node** (`pw-cli create-node adapter-factory ... media.class=Audio/Source/Virtual` or `pactl load-module module-null-sink`), driven by `pw-cat`/`paplay` playing fixtures into the monitor — covers enumeration, open/close idempotency, format renegotiation, device-lost (unload the module mid-stream). Gated behind `#[ignore]`/an env var so `cargo test` stays hermetic; CI job starts a headless `pipewire` + `wireplumber`.
  3. **Latency/conformance checks** in the quickstart (first-frame ≤ 100 ms, close ≤ 200 ms) measured by a small example binary.
- **Rationale**: The mock keeps the correctness suite fast and deterministic; virtual nodes exercise the real negotiation/registry code paths that mocks cannot; matches the spec's independent-test definitions.
- **Alternatives considered**: only mocking (misses real server behavior, the historical source of format bugs per US2), requiring physical hardware in CI (non-deterministic, unavailable).

## R8. Toolchain and packaging

- **Decision**: Rust **stable ≥ 1.85, edition 2024**; Cargo **workspace** at repo root with the library at `crates/audio-adapter` (crate name `myna-audio-adapter`). System build deps: `libpipewire-0.3-dev`, `libpulse-dev`, `clang` (bindgen). Features: `pipewire` (default), `pulse` (default), `vad`, `denoise`, `async`.
- **Rationale**: The repo will host more myna components (Speech Controller, etc.); a workspace from day one avoids restructuring. Feature flags keep the mandatory core (capture + convert) dependency-light per the assumptions about resource constraints.
- **Alternatives considered**: single crate at repo root (blocks future components), splitting backends into separate crates now (premature).
