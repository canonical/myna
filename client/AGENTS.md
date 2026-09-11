# Preface

Read this file before changing the Rust workspace, including capture, session orchestration, desktop integration, HUD behavior, or client settings.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

The client is a Rust workspace that turns activation events into bounded microphone capture, drives a backend session, and injects committed text into the previously focused application. The orchestrator stays independent of concrete transports and desktop services.

# Important

- Verify with `make lint-client test-client` (add `make mutate-client MUTATE='-p <crate> -f <file>'` for a suite whose strength is in doubt); the repository-wide rules are in the root `.kb/verification.md`.

# Directory

- `myna-core/` - Shared audio, event, settings, and wire types.
- `myna-audio/` - Native PipeWire capture and device discovery.
- `myna-orchestrator/` - Session and residency state machines plus boundary traits.
- `myna-cli/` - `myna-dictate` development and testbed binary.
- `myna-desktop/` - Desktop activation, IBus injection, and state publication.
- `myna-hud/` - Focus-safe dictation status renderer.
- `data/` - Shared schemas and packaged client data.

# Documents

- `.kb/audio-capture.md` - Capture ownership, buffering, format, and privacy invariants.
- `.kb/crate-architecture.md` - Cargo dependencies and runtime integration boundaries.
- `.kb/desktop-integration.md` - Activation, focus, injection, and indication behavior.
- `.kb/runtime-settings.md` - Persisted client settings and streaming-mode resolution.
