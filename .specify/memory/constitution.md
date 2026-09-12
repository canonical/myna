<!--
Sync Impact Report
==================
Version change: 1.2.0 → 1.3.0
Modified principles:
  - I. Red-Green TDD — added temporal boundary (from ratification onward);
    added explicit exclusion for evaluation harnesses (Python testbed, myna-server)
Added sections:
  - Python components as evaluation harnesses (under Technology & Environment Constraints)
Removed sections: none
Templates requiring updates:
  - ✅ .specify/templates/plan-template.md — no changes required
  - ✅ .specify/templates/spec-template.md — no changes required
  - ✅ .specify/templates/tasks-template.md — no changes required
  - ⚠ specs/001-rust-audio-adapter/tasks.md — still pending regeneration (already stale
    vs FR-020–FR-022)
Follow-up TODOs: none

Previous reports: 1.2.0 added Staged Delivery in Feature Branches. 1.1.0 added Commit
& Pull Request Communication. 1.0.0 (initial ratification) established Core Principles
I–V, Technology & Environment Constraints, Development Workflow & Quality Gates,
Governance; updated tasks-template.md (tests REQUIRED) and
specs/001-rust-audio-adapter/plan.md (Constitution Check gates).
-->

# Myna Constitution

## Core Principles

### I. Red-Green TDD (NON-NEGOTIABLE)

All new production code written after ratification (2026-07-15) MUST be written
test-first: write a test that captures the required behavior, observe it fail (red), write
the minimum implementation to make it pass (green), then refactor with the suite green.
Existing code written before ratification is not retroactively required to have been test-
first, but any behavioral change to existing code MUST follow the red-green-refactor cycle.
Pull requests that add behavior MUST show test and implementation changes together; a change
that adds behavior without a test that would have failed before the change MUST be rejected
in review. Contract guarantees defined in feature design artifacts (e.g., contracts/) MUST
be encoded as executable tests before the code that satisfies them exists.

**Evaluation harnesses excluded**: the Python testbed (`myna.testbed`) and `myna-server`
are research and evaluation infrastructure, not shipped production components (see
Technology & Environment Constraints). They are explicitly exempt from the TDD
requirement — their tests may be written after implementation or not at all.

Rationale: myna processes live microphone audio where regressions are audible and
privacy-sensitive; test-first is the only reliable way to keep behavioral guarantees
enforced rather than aspirational. The temporal boundary prevents endless retroactive
backfill of tests for working, pre-ratification code, while ensuring all forward momentum
is test-driven.

### II. Integration-Test Readiness on Real Audio Stacks

Every component MUST be designed so its integration tests can run against a real audio
server on (a) a VM equipped with a virtual audio interface (e.g., a PipeWire/PulseAudio
null-sink or loopback node standing in for a microphone) and (b) real hardware, without code
changes — only environment/configuration may differ. Concretely: system boundaries MUST sit
behind swappable interfaces (e.g., backend traits), integration suites MUST be selectable via
environment gating, and test fixtures MUST be injectable through the virtual audio interface.
Hermetic unit tests MUST NOT require an audio server; integration tests MUST NOT require
physical hardware.

Rationale: audio bugs overwhelmingly live at the server boundary (negotiation, timing,
device lifecycle); tests that cannot run on a disposable VM will not run in CI, and tests
that cannot run on hardware will not catch what users hit.

### III. Performance Watermarks and Regression Sensitivity

Performance-relevant code MUST ship with measurements that establish high and low water
marks for resource usage — at minimum: peak and steady-state memory, CPU utilization,
end-to-end latency, and buffer occupancy where applicable. Watermarks MUST be recorded as
checked-in baselines on defined reference environments, and performance tests MUST be
sensitive enough to flag deviations beyond an explicitly declared tolerance per metric
(tolerances chosen to detect meaningful drift, not just order-of-magnitude breakage).
A change that moves a watermark past its tolerance MUST either be justified and the baseline
re-ratified in the same PR, or be fixed before merge.

Rationale: a dictation pipeline has hard latency budgets and runs continuously on user
desktops; without sensitive baselines, resource creep accumulates invisibly until the
product misses its 100 ms-class targets.

### IV. Workshop-Based Development Environment

The canonical development environment MUST be defined with Canonical Workshop
(https://ubuntu.com/workshop/docs): a `workshop.yaml` at the repository root declares the
environment as composable SDKs, and contributors launch it with `workshop launch` /
`workshop exec`. All toolchain and system dependencies (Rust toolchain, libpipewire/libpulse
headers, audio utilities) MUST be expressible through the Workshop definition; documentation
MUST NOT require ad-hoc host mutations beyond installing Workshop itself. Required host
resources (audio, GPU, mounts) MUST be declared as Workshop interfaces. CI SHOULD consume
the same definition or a derivation of it so local and CI environments do not drift.

Rationale: repeatable, ephemeral environments eliminate "works on my machine" drift and make
the VM-based integration story (Principle II) a first-class, shareable artifact.

### V. Privacy-First, Offline-First Audio Handling

Captured audio MUST NOT be persisted to disk by default; all intermediate audio MUST live in
bounded in-memory buffers that are cleared when their stream or session ends. No component
may require network connectivity for its core function; there MUST be no silent fallback to
remote services. Diagnostics MUST NOT include raw audio or full transcription content unless
the user has explicitly opted in.

Rationale: myna's product promise (docs/architecture) is local, private dictation; this is
the constraint most expensive to retrofit and most damaging to violate.

## Technology & Environment Constraints

- Implementation language for shipped system components is Rust (stable toolchain, current
  edition); deviations require Complexity Tracking justification in the feature plan.
- Target platform is Ubuntu Desktop (current LTS and later) with PipeWire as the primary
  audio server and PulseAudio compatibility maintained.
- **Python components are evaluation harnesses**: the Python testbed (`myna.testbed`) and
  `myna-server` are research and evaluation infrastructure — they benchmark candidate
  adapters, measure accuracy and latency, and serve as a lightweight local backend during
  development. They are NOT shipped production components and are not subject to the TDD
  requirement (Principle I), performance watermark baselines (Principle III), or the Rust
  language constraint. The production inference snap and dictation client are Rust; the
  Python components are scaffolding for the research that informs them.
- New system dependencies MUST be added to the Workshop definition in the same PR that
  introduces them.
- Reference environments for watermark baselines (Principle III) are the Workshop container
  for hermetic metrics and the virtual-audio VM profile for integration metrics; hardware
  tiers may be added as additional named baselines.

## Development Workflow & Quality Gates

- Every feature plan MUST pass the Constitution Check gate before Phase 0 research and again
  after Phase 1 design; violations require a Complexity Tracking entry or a redesign.
- Task lists generated for features MUST order test tasks before their corresponding
  implementation tasks (red before green); test tasks are never optional for behavior-bearing
  code.
- CI MUST run, at minimum: hermetic unit/contract suites on every PR; virtual-audio
  integration suites on every PR or merge queue; performance watermark checks with declared
  tolerances before release branches move.
- Code review MUST verify: red-green evidence (Principle I), integration-readiness of new
  boundaries (Principle II), watermark impact (Principle III), Workshop definition currency
  (Principle IV), and audio-privacy invariants (Principle V).

### Staged Delivery in Feature Branches

- Implementation MUST be staged across sensibly scoped feature branches: each branch delivers
  one coherent, independently testable increment (typically one user story, one architectural
  layer, or one cross-cutting gate) — never a monolithic drop of the whole feature.
- Branch scope MUST follow the task plan's execution order: a branch contains its increment's
  tests and implementation together (red-green within the branch), and its declared
  prerequisites MUST already be merged — a branch MUST NOT build on unmerged sibling work.
- Every merge MUST leave the default branch green: the hermetic suites pass, and the
  integration/performance gates required for the increment's scope pass. Work that would
  break the default branch stays on its feature branch until it doesn't.
- Task planning (tasks.md) MUST include a branch staging plan: which phases/stories map to
  which branches, their merge order, and which test gates (hermetic, sandbox integration,
  performance watermarks) apply at each merge.
- Rationale: staged branches keep review scope small, make the red-green evidence
  (Principle I) legible per increment, and let integration and performance gates run at
  meaningful checkpoints instead of once at the end.

### Commit & Pull Request Communication

- Commit messages and PR descriptions MUST NOT attribute authorship to AI agents or tools
  (no agent co-author trailers, "generated with" footers, or similar attribution).
- Commit messages and PR descriptions MUST briefly state what was done and why. They MUST NOT
  describe implementation details unless those details are nontrivial (e.g., a non-obvious
  algorithm choice, a subtle concurrency or compatibility constraint) — the diff carries the
  trivial detail.
- Rationale: history should read as a concise record of intent; attribution noise and
  restated diffs dilute it.

## Governance

This constitution supersedes ad-hoc practice for all myna components. Amendments are made by
PR that modifies this file, states the semantic version bump and its rationale in the Sync
Impact Report comment, and updates all dependent templates and in-flight feature plans in the
same change.

Versioning policy: MAJOR for removing or redefining a principle in a backward-incompatible
way; MINOR for adding a principle or materially expanding guidance; PATCH for clarifications
and wording. Compliance is reviewed at every feature's Constitution Check gate and in code
review per the workflow gates above; persistent violations MUST be either remediated or
ratified as amendments — silent divergence is not permitted.

**Version**: 1.3.0 | **Ratified**: 2026-07-15 | **Last Amended**: 2026-07-15
