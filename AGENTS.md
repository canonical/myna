# Preface

Use this file for repository-wide orientation. Before changing a subsystem, read its local `AGENTS.md` and only the knowledge documents relevant to the task.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Myna is an offline speech-to-text system for Ubuntu Desktop. A Rust client captures microphone audio, drives dictation sessions, and injects committed text into the focused application. Inference backends run in strictly confined snaps and receive audio over a Unix socket.

The repository also contains model evaluation, benchmarking, packaging, and desktop integration tooling. Durable agent knowledge belongs in nearby `.kb/` documents; user instructions remain in README files; merged feature specifications under `specs/` are historical snapshots rather than live documentation.

# Important

- A change is done when `make check` and `make test-<component>` are green for every component touched, and `make coverage` passes its patch gate when logic was added or moved. `make preflight` is the merge bar. The rules, the red/green test discipline and when to reach for mutation testing are in `.kb/verification.md`.

# Architecture

Myna has three primary boundaries:

- The Rust workspace in `client/` owns capture, orchestration, desktop activation, text injection, and user settings.
- The Python project in `server/` owns the shared server contract, inference adapters, server process, testbed, and benchmarker.
- The `*-snap/` directories package the orchestrator and inference backends. Model weights and runtimes are snap components.

The client pushes PCM to a backend; inference snaps never access the microphone. Both language implementations express the same session contract, but code and tests are the source of truth for wire details.

# Directory

- `client/` - Rust workspace for the dictation client and desktop integration.
- `server/` - Python inference server, adapters, testbed, and benchmarker.
- `*-snap/` - Snap packaging for the client and each inference family.
- `extensions/` - GNOME Shell integration.
- `dev/` - Development, packaging, benchmark, and quality-gate scripts.
- `docs/` - Human-run system test plans and multilingual test passages.
- `specs/` - Frozen feature-design artifacts retained for historical context.
- `tests/` - Confined end-to-end tests.

# Documents

- `.kb/agents.md` - Rules for reading and maintaining the agent knowledge base.
- `.kb/repository-layout.md` - Repository boundaries and placement rules.
- `.kb/session-contract.md` - Durable cross-language session and streaming semantics.
- `.kb/system-architecture.md` - High-level runtime components and trust boundaries.
- `.kb/verification.md` - What done means: the gates, patch coverage, red/green tests and scoped mutation testing.
- `client/AGENTS.md` - Rust client architecture and local knowledge.
- `server/AGENTS.md` - Python server, inference, packaging, and benchmark knowledge.
- `docs/AGENTS.md` - Scope and maintenance rules for human test documentation.
