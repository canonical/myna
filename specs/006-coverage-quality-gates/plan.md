# Implementation Plan: Coverage, Dead-Code Visibility, and Quality Gates

**Branch**: `[006-coverage-quality-gates]` | **Date**: 2026-07-26 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/006-coverage-quality-gates/spec.md`

## Summary

Give myna trustworthy, enforced coverage and hygiene signal with zero external
services: one Workshop command per language produces browsable + machine-readable
coverage (cargo-llvm-cov for the Rust workspace; pytest-cov/coverage.py for the
Python side, both already partially configured); a scripted use-case exercise
runs the shipped binaries under the same instrumentation and merges with test
coverage to classify every region as test-covered / use-case-only / never-executed,
feeding a dead-code report (dynamic + vulture/cargo-machete statics). CI gains a
coverage job whose patch gate is self-hosted (`diff-cover` against the PR base on
Cobertura exports, 80% default), plus expanded static checks (fmt, cargo-deny
bans codifying the offline invariant, ruff, scoped mypy, shellcheck, actionlint)
and a scheduled advisory audit. A time-boxed spread spike concludes with an
adopt-or-reject record; adoption's first suite is a confined snap-to-snap
dictation over the content share on a clean 24.04 VM with virtual audio.

## Technical Context

**Language/Version**: Rust 1.75+ (workspace `client/`), Python ≥3.12 (`server/`, `uv`),
GJS (extension — no new checks beyond existing `gjs-test`), Bash/YAML (CI, Workshop, dev scripts)

**Primary Dependencies**: cargo-llvm-cov + llvm-tools-preview (Rust coverage);
pytest-cov/coverage.py (already in `server` dev group; branch + source configured);
diff-cover (self-hosted patch gate); ruff, mypy, vulture, pip-audit (Python statics);
cargo-deny, cargo-machete, cargo-audit (Rust statics); shellcheck, actionlint (repo hygiene);
spread (evaluation only, pinned upstream commit)

**Storage**: Files only — coverage data (`.coverage*`, `*.profraw`), reports
(`client/target/coverage/`, `server/htmlcov/`, Cobertura/lcov XML), decision record under `specs/006-*/`

**Testing**: Existing suites drive coverage — `cargo test --workspace` (hermetic +
`MYNA_PIPEWIRE_TESTS`/desktop-gated integration), `uv run pytest` (offline; real-model/GPU skip),
`gjs-test` (unchanged). Use-case exercise scripts live in `dev/` and run the shipped binaries.

**Target Platform**: Ubuntu 24.04 (Workshop env + GH hosted runners); spread spike adds
qemu VMs (ubuntu-24.04-64, KVM on `ubuntu-latest` runners)

**Project Type**: Developer tooling / CI infrastructure for an existing multi-language repo

**Performance Goals**: Coverage overhead ≤ 2× existing CI job time (SC-001); patch gate
adds < 1 min; scheduled audits run weekly off the PR path

**Constraints**: No coverage data leaves CI (self-hosted gate, spec decision 2026-07-26);
no audio/transcription content in reports or artifacts (constitution V); all new dev
commands are named Workshop actions consumed identically by CI (constitution IV);
reports must distinguish gated-but-skipped populations from genuinely dead code

**Scale/Scope**: ~5 Rust crates, ~1 Python package + tests, 3 workflows (ci.yml, snap.yml,
docs-lint.yml), ~15 dev scripts; no changes to shipped component behavior expected

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Verdict | Notes |
|-----------|---------|-------|
| I. Red-Green TDD | **Pass (out of scope)** | No shipped production code changes. CI config, Workshop actions, `dev/` scripts, and evaluation-harness Python are exempt tiers (constitution: Python testbed/myna-server exemption; CI/tooling is not shipped component code). Any testability-seam change to Rust components that proves necessary follows red-green. |
| II. Integration-Test Readiness | **Pass — reinforced** | The use-case exercise (FR-004) and the spread spike (FR-015) both run on virtual audio (null/virtual source, fake adapter, WAV fixtures) with environment-only differences vs hardware — exactly the swappable-interface/virtual-audio pattern this principle mandates. Hermetic suites remain audio-server-free. |
| III. Performance Watermarks | **Pass (N/A)** | Feature adds no performance-relevant runtime code. SC-001 bounds CI-time overhead instead (≤2×), which is the relevant budget here. |
| IV. Workshop-Based Environment | **Pass — required by FR-013** | All new commands (coverage, exercise, dead-code, patch gate, new lints) are named actions in `.workshop/myna.yaml`; CI calls those same actions. New tools (llvm-cov, deny, ruff, …) are added to the Workshop SDKs in the same change that uses them. |
| V. Privacy-First, Offline-First | **Pass — extended** | Self-hosted patch gate keeps coverage data in CI (no SaaS). Reports carry code-structure metadata only — never audio or transcription content (FR-016). The cargo-deny ban list (FR-011) newly *enforces* the offline invariant for the shipped client. Use-case runs use fixtures; nothing persists audio. |

**Post-Phase-1 re-check**: No design artifact below introduces a violation; the two
privacy-sensitive design choices (self-hosted gate; fixture-only exercise data) are
recorded in research.md. Gate stands: **PASS**, no Complexity Tracking entries.

## Project Structure

### Documentation (this feature)

```text
specs/006-coverage-quality-gates/
├── plan.md              # This file
├── research.md          # Phase 0 output — tool decisions and known wrinkles
├── data-model.md        # Phase 1 output — coverage populations, gate verdicts
├── quickstart.md        # Phase 1 output — validation walkthrough
├── contracts/
│   ├── workshop-actions.md   # Named-command contracts (inputs/outputs/exit codes)
│   └── ci-gates.md           # Patch-gate + static-check pass/fail contracts
└── checklists/
    └── requirements.md  # Spec quality checklist (complete)
```

### Source Code (repository root)

```text
.workshop/
└── myna.yaml            # NEW named actions: cov, py-cov, exercise, deadcode,
                         # patch-cov, plus extended lint actions (tool installs
                         # ride the existing rust/uv SDK definitions)

dev/
├── exercise.sh          # NEW — scripted use-case runs under instrumentation
                         # (fake-adapter sessions ×2 dialects, WAV, live-mic gated,
                         # desktop publisher)
├── coverage_populations.py  # NEW — merges Cobertura exports; classifies
                         # test-covered / use-case-only / never-executed; emits
                         # dead-code summary (consumes vulture + machete output)
└── vulture_allowlist.py # NEW — dynamic entry points (adapters-by-name, fixtures)

.github/workflows/
├── ci.yml               # EXTENDED — coverage job (Workshop actions + diff-cover
                         # gate + artifacts), static-checks job
├── audit.yml            # NEW — scheduled cargo-audit + pip-audit (weekly)
└── snap.yml             # unchanged until spread decision; superseded smoke if adopted

spread.yaml              # NEW (only if spike decides adopt) — qemu backend, 24.04
tests/spread/            # NEW (adopt-only) — confined-e2e suite
```

**Structure Decision**: No new source tree — this feature extends existing
locations. Tooling scripts go in `dev/` (repo convention for harness tooling,
evaluation-harness tier per constitution); orchestration in `.workshop/myna.yaml`
(constitution IV single-command-source); gates in `.github/workflows/`; spread
artifacts at repo root per spread convention, gated on the US5 decision record.

## Complexity Tracking

> No constitution violations — section intentionally empty.
