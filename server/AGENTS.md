# Preface

Read this file before changing the Python server, inference adapters, testbed, benchmarker, or inference snap packaging.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

The server project exposes model adapters through one session contract and also provides the offline evaluation and benchmark tooling. Production deployments package the same server code and model-specific extras into inference snaps.

# Directory

- `src/myna/core/` - Session, event, audio, capability, and transport contract.
- `src/myna/server/` - Server process and adapter loading.
- `src/myna/testbed/` - Candidate adapters, corpus handling, metrics, and harnesses.
- `src/myna/benchmarker/` - Reproducible benchmark planning, execution, and reporting.
- `tests/` - Contract, adapter, packaging, and benchmark tests.
- `fixtures/` - Generated synthetic fixtures for plumbing tests, not accuracy claims.

# Documents

- `.kb/backend-configuration.md` - Provisioning, persistent configuration, and session-parameter boundaries.
- `.kb/inference-packaging.md` - Shared inference snap structure and packaging invariants.
- `.kb/inference-snap-architecture.md` - Common inference snap control and serving paths.
- `.kb/benchmarking.md` - Safe benchmark workflow and result interpretation.
