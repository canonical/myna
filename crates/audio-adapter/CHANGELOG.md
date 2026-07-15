# Changelog

All notable changes to `myna-audio-adapter` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate structure and Cargo workspace integration.
- Public API: `enumerate_nodes()` and `open_stream()` with stateless stream primitives.
- PipeWire and PulseAudio backend placeholders behind feature flags.
- `MockBackend` for hermetic unit and contract tests.
- Audio format descriptions and a configurable target format (default 16 kHz mono S16LE).
- Bounded in-memory `AudioQueue` with drop-oldest overrun and silence-fill underrun handling.
- Raised-cosine fade smoothing at splice boundaries to avoid clicks.
- Format conversion pipeline supporting S16LE/F32LE and arbitrary channel counts.
- `rubato`-based fallback resampler when the server cannot deliver the target rate.
- `PreprocessStage` trait and feature-gated stubs for RNNoise denoising and Silero VAD.
- Optional `futures::Stream` adapter behind the `async` feature.
- `capture_check` and `preprocess_check` example binaries.
- Contract tests covering target-format frames, contiguous timeline, idempotent open, close latency, device loss, and no-device errors.
- Consumer-scenario test matching the Speech Controller call pattern.
- Architectural diagrams in `docs/diagrams.md`.

### Known Limitations

- PipeWire and PulseAudio backends are stubs; real capture implementation requires a build environment with `libpipewire-0.3-dev` and `libpulse-dev`.
- Denoise and VAD stages are pass-through stubs pending API integration.
- Dereverberation is deferred to a future stage.
