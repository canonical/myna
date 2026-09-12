# Preface

Read this document when deciding where code or project knowledge belongs, or when changing boundaries between the Rust client, Python server, and snap packages.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Myna is intentionally polyglot. The production client uses Rust for native desktop and audio integration; inference and model evaluation use Python because the supported ML runtimes are Python-oriented. Snap packaging is the deployment boundary between them.

# Architecture

## Rust client

`client/` is one Cargo workspace:

- `myna-core` defines audio, session, event, settings, and wire types shared by Rust client components.
- `myna-audio` owns native PipeWire capture.
- `myna-orchestrator` owns the wire-agnostic session and model-residency state machines.
- `myna-cli` provides the `myna-dictate` development and testbed client.
- `myna-desktop` owns desktop activation and text injection.
- `myna-hud` renders dictation state without taking keyboard focus.

External boundaries are traits with test doubles. Keep transport, capture, activation, injection, and indication replaceable without changing the orchestrator.

## Python server

`server/` is one `uv`-managed project:

- `myna.core` defines the server-side session contract and transports.
- `myna.server` exposes inference adapters through `myna-server`.
- `myna.testbed` contains adapters, corpus handling, metrics, and benchmark support.
- `myna.benchmarker` runs reproducible local and remote benchmark sweeps.

Model-specific dependencies stay behind optional extras and must not leak into `myna.core`.

## Packaging

Each inference family has its own `*-snap/` directory. The snap packages `myna-server`, one or more engines, and model/runtime components. The `myna-snap/` package contains the client-side product.

# Important

- Keep Python `myna.core` and Rust `myna-core` as peer implementations; do not join their build graphs.
- Treat code, schema, and parity tests as authoritative for protocol details.
- Put durable subsystem knowledge beside that subsystem instead of adding root-level design notes.
- Treat merged `specs/NNN-*` content as a design-time snapshot. Do not update it as live documentation.
- Do not add task trackers, meeting notes, implementation diaries, or superseding documents to the knowledge base.
