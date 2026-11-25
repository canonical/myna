# Preface

Read this file before changing the Python server, inference adapters, testbed, benchmarker, or inference snap packaging.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

The server project exposes model adapters through one session contract and also provides the offline evaluation and benchmark tooling. Production deployments package the same server code and model-specific extras into inference snaps.

# Important

- Verify with `make lint-server test-server` (add `make mutate-server MUTATE='<module glob>*'` for a suite whose strength is in doubt); the repository-wide rules are in the root `.kb/verification.md`. Tests that read the repository outside `server/` carry the `repo_tree` marker.

# Directory

- `src/myna/core/` - Session, event, audio, capability, and transport contract.
- `src/myna/server/` - Server process and adapter loading.
- `src/myna/text/` - The `myna-text-v1` provider shared by the text snaps: protocol, server loop, component discovery with registrable kinds, the `myna-text` daemon and the disfluency tagger.
- `src/myna/translation/` - The `myna-translation` daemon and the Bergamot worker; imports no onnxruntime.
- `src/myna/testbed/` - Candidate adapters, corpus handling, metrics, and harnesses.
- `src/myna/benchmarker/` - Reproducible benchmark planning, execution, and reporting.
- `tests/` - Contract, adapter, packaging, and benchmark tests. Shared test-only helpers live in `tests/_*.py`, importable by bare name from any subdirectory; code without a production caller belongs there, not in `src/`.
- `fixtures/` - Generated synthetic fixtures for plumbing tests, not accuracy claims.

# Documents

- `.kb/backend-configuration.md` - Provisioning, persistent configuration, and session-parameter boundaries.
- `.kb/inference-packaging.md` - Shared inference snap structure and packaging invariants.
- `.kb/inference-snap-architecture.md` - Common inference snap control and serving paths.
- `.kb/benchmarking.md` - Safe benchmark workflow and result interpretation.
- `.kb/model-evaluation.md` - How to evaluate a model or a numerics change, Parakeet GPU and keyword-biasing findings, why Whisper takes no prompt keywords, keyword correction after the decode and how it keeps streaming and latency whole (Whisper, SenseVoice, Parakeet), the Nemotron streaming ONNX adapter (export, parity, caption results), and the IndicConformer Hindi adapter (language cut, mel, results).
- `.kb/text-processing-evaluation.md` - What was measured about rules, taggers and small models for the text steps: the run 1 and blind run 2 corpora, metrics, the verdict per approach and operation, overfitting, caveats, usable and unusable assets, and how to rerun with `myna-bench text --eval`.
