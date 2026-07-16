# Implementation Plan: Audio Adapter Library

**Branch**: `001-rust-audio-adapter` | **Date**: 2026-07-15 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/001-rust-audio-adapter/spec.md`

## Summary

A Rust library (`myna-audio-adapter`) that captures audio from PipeWire/PulseAudio, converts it to a consumer-configured target format (default 16 kHz mono S16LE), and delivers contiguous, artifact-free frames through stateless open/read/close stream primitives — preparing microphone input for the myna dictation pipeline's STT inference backend. Technical approach (from research): a backend trait with a native PipeWire primary implementation and PulseAudio fallback; server-side format negotiation with a `rubato`-based in-process fallback; a lock-free bounded ring buffer with drop-oldest overrun and silence-filled underrun handling (smoothed splices); feature-gated Silero VAD and RNNoise preprocessing stages.

## Technical Context

**Language/Version**: Rust stable ≥ 1.85, edition 2024

**Primary Dependencies**: `pipewire` (pipewire-rs, primary backend), `libpulse-binding` (fallback backend), `rubato` (fallback resampler), `ringbuf` (SPSC ring buffer); feature-gated: `voice_activity_detector` (Silero VAD via onnxruntime), `nnnoiseless` (RNNoise), `futures-core` (async adapter)

**Storage**: N/A — bounded in-memory buffers only; audio is never persisted to disk (FR-007)

**Testing**: `cargo test` — unit tests against a `MockBackend` with WAV fixtures; integration tests against real PipeWire using virtual null-sink nodes (env-gated); latency conformance via example binaries (see research R7, quickstart.md). The full suite also runs in a Canonical Workshop sandbox (`workshop.yaml`) with an isolated audio server — PipeWire or PulseAudio selected at launch — and virtual input devices, host audio untouched (FR-021/FR-022, SC-007/SC-008)

**Target Platform**: Ubuntu Desktop 24.04+ on Linux (Wayland); any Linux with PipeWire ≥ 1.0 or PulseAudio

**Project Type**: Rust library crate in a new Cargo workspace (first crate of the myna repo)

**Performance Goals**: first frame ≤ 100 ms after open (SC-001); end-to-end delivery lag ≤ 100 ms (SC-003); close/release ≤ 200 ms (SC-004); capture callback allocation- and lock-free

**Constraints**: bounded buffers (default 10 s cap, configurable); no disk persistence of audio; no network access (SC-006); no audible artifacts at frame/splice boundaries (FR-015); preprocessing optional and off by default

**Scale/Scope**: single desktop process; ≤ a handful of concurrent streams (one per input node, FR-003); ~32 KB/s per stream at target format

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated against Myna Constitution v1.2.0 (staged-delivery gate satisfied by the Branch Staging Plan in tasks.md):

- **I. Red-Green TDD**: contract guarantees G1–G14 (contracts/audio-adapter-api.md) are directly assertable as failing tests before implementation; tasks.md MUST order test tasks before implementation tasks. PASS
- **II. Integration-test readiness (VM w/ virtual audio interface or real hardware)**: all server interaction sits behind the `AudioBackend` trait; integration suite is env-gated (`MYNA_AUDIO_IT=1`) and driven through a virtual null-sink node (quickstart.md), so the identical tests run on a VM with a virtual audio interface or on real hardware with no code changes; unit/contract suites are hermetic via `MockBackend`. PASS
- **III. Performance watermarks & regression sensitivity**: performance goals quantified (SC-001/003/004); `capture_check` measures latency conformance. Watermark baselines (peak/steady memory, CPU, latency, buffer occupancy) with declared per-metric tolerances MUST be added as explicit tasks in Phase 2 planning. PASS (with tasking obligation)
- **IV. Workshop-based development environment**: repo has no `workshop.yaml` yet; creating it (Rust toolchain, libpipewire/libpulse dev headers, audio utilities, audio interface declaration) is a required Setup task for this feature. FR-021/FR-022 additionally make the Workshop sandbox a spec-mandated test execution mode with launch-time PipeWire/PulseAudio backend selection, host isolation, and clean teardown. PASS (with tasking obligation)
- **V. Privacy-first, offline-first**: bounded in-memory buffers only, no disk persistence (FR-007, G13), no network paths (SC-006, G14). PASS

**Post-Phase-1 re-check**: single library crate, only two trait abstractions (`AudioBackend` mandated by FR-001 dual-server support; `PreprocessStage` mandated by optional/deferred stages FR-010/FR-011); no gate violations. PASS — Complexity Tracking table left empty.

## Project Structure

### Documentation (this feature)

```text
specs/001-rust-audio-adapter/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── diagrams.md          # Phase 1 output — architecture block diagram + sequence/flow diagrams (FR-019)
├── contracts/
│   └── audio-adapter-api.md   # Public Rust API contract (guarantees G1–G14)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)

```text
Cargo.toml                      # new workspace manifest
workshop.yaml                   # Canonical Workshop dev-environment definition (constitution IV)
crates/audio-adapter/
├── Cargo.toml                  # crate: myna-audio-adapter; features: pipewire, pulse, vad, denoise, async
├── src/
│   ├── lib.rs                  # public facade: enumerate_nodes(), open_stream()
│   ├── error.rs                # Error enum (NoDevice, PermissionDenied, UnsupportedFormat, DeviceLost, Backend)
│   ├── config.rs               # StreamConfig, PreprocessConfig, NodeSelector, BackendSelector
│   ├── format.rs               # AudioFormat, SampleFormat
│   ├── node.rs                 # InputNode, NodeId
│   ├── frame.rs                # AudioFrame, StreamItem, StreamEvent
│   ├── stream.rs               # AudioStream: read/read_timeout/close, ring-buffer consumer side
│   ├── ring.rs                 # bounded SPSC ring, drop-oldest, overrun/underrun accounting, splice smoothing
│   ├── convert/
│   │   ├── mod.rs              # conversion pipeline (bypass when server delivers target format)
│   │   ├── resample.rs         # rubato-backed fallback resampler
│   │   └── channels.rs         # channel mixdown + sample-format conversion
│   ├── preprocess/
│   │   ├── mod.rs              # PreprocessStage trait + stage chain
│   │   ├── denoise.rs          # nnnoiseless stage (feature = "denoise")
│   │   └── vad.rs              # Silero VAD stage (feature = "vad")
│   ├── backend/
│   │   ├── mod.rs              # AudioBackend trait + Auto probe (PipeWire → Pulse)
│   │   ├── pipewire.rs         # native PipeWire backend (feature = "pipewire")
│   │   ├── pulse.rs            # PulseAudio fallback backend (feature = "pulse")
│   │   └── mock.rs             # MockBackend (cfg(test) / test-util feature)
│   └── async_stream.rs         # futures::Stream adapter (feature = "async")
├── examples/
│   ├── capture_check.rs        # latency/lifecycle conformance (quickstart)
│   └── preprocess_check.rs     # VAD/denoise validation (quickstart)
└── tests/
    ├── contract.rs             # G1–G15 assertions against MockBackend
    ├── consumer_scenario.rs    # Speech Controller call pattern end-to-end (FR-020, G15):
    │                           #   enumerate → open → read loop w/ events → close
    ├── integration.rs          # real-PipeWire tests, env-gated (MYNA_AUDIO_IT=1)
    └── fixtures/               # WAV fixtures (speech_48k_stereo.wav, noisy_speech.wav, …)
```

**Structure Decision**: New Cargo workspace at repo root with the library under `crates/audio-adapter`, since the repository currently contains only documentation and this is the first of several planned myna components (Speech Controller, Text Injection Layer per docs/architecture). The crate is self-contained; all system integration happens behind the `AudioBackend` trait so contract tests run hermetically.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

*(none — no gate violations)*
