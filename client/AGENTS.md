# Preface

Read this file before changing the Rust workspace, including capture, session orchestration, desktop integration, HUD behavior, or client settings.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

The client is a Rust workspace that turns activation events into bounded microphone capture, drives a backend session, and injects committed text into the previously focused application. The orchestrator stays independent of concrete transports and desktop services.

# Important

- The toolchain is pinned in `rust-toolchain.toml` for the workshops, CI and the client snap; bump it in its own commit and fix what `make check` reports. The myna-config deb builds with each series' distro rustc instead (1.91 on noble), so code must not need anything newer.
- Verify with `make lint-client test-client` (add `make mutate-client MUTATE='-p <crate> -f <file>'` for a suite whose strength is in doubt); the repository-wide rules are in the root `.kb/verification.md`.

# Directory

- `myna-core/` - Shared audio, event, settings, and wire types, plus the language-to-model recommendation.
- `myna-audio/` - Native PipeWire capture, device discovery and cue playback.
- `myna-orchestrator/` - Session and residency state machines plus boundary traits.
- `myna-cli/` - `myna-testbed` development binary.
- `myna-desktop/` - Desktop activation, IBus injection, and state publication.
- `myna-hud/` - Focus-safe dictation status renderer.
- `myna-config/` - Myna Settings, the unconfined GTK onboarding and configuration application.
- `data/` - Shared schemas and packaged client data.
- `build-support/` - Build-script logic shared across crates: the `MYNA_VERSION` the binaries report, `dev/version.sh`'s.

# Documents

- `.kb/audio-capture.md` - Capture ownership, buffering, format, and privacy invariants.
- `.kb/crate-architecture.md` - Cargo dependencies and runtime integration boundaries.
- `.kb/desktop-integration.md` - Activation, focus, injection, and indication behavior.
- `.kb/model-recommendation.md` - How the recommended model family follows the user's language.
- `.kb/runtime-settings.md` - Persisted client settings, streaming mode, and live reload.
- `myna-config/AGENTS.md` - Privilege paths, plan executor, and refresh budget of Myna Settings.
