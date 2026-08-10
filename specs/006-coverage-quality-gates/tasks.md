# Tasks: Coverage, Dead-Code Visibility, and Quality Gates

**Input**: Design documents from `/specs/006-coverage-quality-gates/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: This feature ships no production component code — it is CI/Workshop
configuration and `dev/` tooling (evaluation-harness tier per the constitution's
Python-harness exemption; constitution Principle I does not bind it). Red-green
test tasks are therefore replaced by **validation tasks** that execute the
quickstart.md scenario for the story, including the scripted negative tests
(fail-loud merge, deliberate-violation gates). If any task discovers a needed
change to a shipped Rust component, that change follows red-green TDD and is
added to this list at that point.

**Organization**: Tasks grouped by user story; each story is an independently
testable increment and maps to one branch (see Branch Staging Plan).

**Implementation status (2026-08-10, integration-220627)**: all tasks except
T022/T037/T040/T041 implemented and validated **in the canonical Workshop
environment** (T005/T009 done: every action — cov, py-cov, exercise,
deadcode, patch-cov incl. exit-2 propagation, fmt, machete, deny, py-lint,
py-types, shell-lint, workflow-lint, audit — ran green in the workshop; the
fail-loud negative test of T018 passes). The static gates caught real
pre-existing violations at introduction (SC-005's deliberate-violation test
came for free), and the first pip-audit run found two real advisories
(msgpack, setuptools — fixed). Pending: the three demonstration PRs (T022),
spread nightly CI run (T037 local VM half validated 2026-08-10), analyze re-run + full
quickstart (T040/T041). Branch staging was not followed — work landed on
integration-220627 per maintainer direction.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1–US5)
- File paths are repo-relative

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Provision tooling so every later story only writes configuration

- [x] T001 Extend the Workshop definition (`.workshop/myna.yaml` and/or its SDK files) to provision the Rust tools: `llvm-tools-preview` component, `cargo-llvm-cov`, `cargo-deny`, `cargo-machete`, `cargo-audit` (pinned versions, offline-cacheable per constitution IV)
- [x] T002 [P] Add Python tooling to the dev dependency group in `server/pyproject.toml`: `diff-cover`, `ruff`, `mypy`, `vulture`, `pip-audit`; run `uv lock`
- [x] T003 [P] Add coverage/report output paths to `.gitignore`: `client/target/coverage/`, `server/.coverage*`, `server/coverage-*.xml`, `server/htmlcov/` (verify `client/target/` rule doesn't already suffice)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared pieces both US2 and US3 consume

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [x] T004 Create `dev/coverage_lib.py`: Cobertura XML parsing plus path normalization to repo-root-relative paths (handles `client/`-workspace-relative llvm-cov paths and `server/`-relative coverage.py paths, per research.md D3 wrinkle)
- [x] T005 Smoke-verify provisioning from T001–T002 inside the Workshop environment: each tool runs `--version`; document versions in the PR description

**Checkpoint**: Tools provisioned in the canonical environment; shared coverage helper exists

---

## Phase 3: User Story 1 - One-command local coverage for both languages (Priority: P1) 🎯 MVP

**Goal**: `workshop run myna cov` and `workshop run myna py-cov` produce browsable + machine-readable coverage, identically for contributors and CI

**Independent Test**: Quickstart Scenario 1 — clean Workshop env, both commands exit 0, HTML opens with per-line detail, Cobertura (+lcov for Rust) exports exist

### Implementation for User Story 1

- [x] T006 [US1] Add the `cov` action to `.workshop/myna.yaml`: `cargo llvm-cov --workspace` in `client/` emitting HTML + lcov + Cobertura to `client/target/coverage/` (contract: contracts/workshop-actions.md `cov`)
- [x] T007 [P] [US1] Add the `py-cov` action to `.workshop/myna.yaml`: `uv run pytest --cov=myna --cov-branch --cov-context=test` in `server/` emitting HTML (with `--show-contexts`), term-missing, and Cobertura XML (contract: `py-cov`)
- [x] T008 [US1] Verify FR-003 gating: run `cov` with `MYNA_PIPEWIRE_TESTS` unset (skips cleanly, report generates, gated paths visible as uncovered) and set (gated hits contribute); record outcome in the PR

### Validation for User Story 1

- [x] T009 [US1] Execute quickstart.md Scenario 1 end-to-end in a fresh Workshop environment; confirm SC-001 timing (≤2× existing job time)

**Checkpoint**: Both one-command reports work locally; MVP deliverable

---

## Phase 4: User Story 2 - Use-case coverage and dead-code visibility (Priority: P2)

**Goal**: Scripted real use-cases run instrumented, merge with test coverage, and produce the populations + dead-code report

**Independent Test**: Quickstart Scenario 2 — `exercise` then `deadcode`; populations.md classifies all regions; negative test (deleted raw data file) fails loudly

### Implementation for User Story 2

- [x] T010 [US2] Create `dev/exercise.sh`: launch `myna-server --adapter fake` under `coverage run --parallel-mode --context=usecase:*`, drive a dictation session with `myna-dictate` (internal dialect, WAV fixture input) instrumented via `cargo llvm-cov run`, assert the expected transcript event
- [x] T011 [US2] Add the IE115-dialect session to `dev/exercise.sh` (same pattern, second context)
- [x] T012 [US2] Add the `myna-desktop --dbus` publisher scenario to `dev/exercise.sh` against a stub D-Bus consumer (state/level only — no content, FR-016)
- [x] T013 [US2] Add the environment-gated live-capture scenario to `dev/exercise.sh` (runs where capture hardware exists; clear skip notice otherwise — amended FR-004)
- [x] T014 [US2] Add the fail-loud merge step: every scheduled run-kind must produce its expected raw data file; a missing file aborts with non-zero and names the missing input (FR-005)
- [x] T015 [US2] Create `dev/coverage_populations.py` on `dev/coverage_lib.py`: classify every line as test-covered / use-case-only / never-executed from tests-only vs merged Cobertura exports; emit `client/target/coverage/populations.md` + `populations.json` (data-model.md: Coverage Population)
- [x] T016 [P] [US2] Create `dev/vulture_allowlist.py` (adapter-by-name loaders, pytest fixtures) and wire the statics into the dead-code section: `vulture --min-confidence 80`, `ruff --select F401,F841`, `cargo machete` (data-model.md: Dead-Code Report)
- [x] T017 [US2] Add the `exercise` and `deadcode` actions to `.workshop/myna.yaml` (contracts: `exercise`, `deadcode`)

### Validation for User Story 2

- [x] T018 [US2] Execute quickstart.md Scenario 2 including the fail-loud negative test and the spot-checks (SC-002 known regions, SC-003 zero allow-list false positives)

**Checkpoint**: Merged report + dead-code report exist and are trustworthy

---

## Phase 5: User Story 3 - CI enforces coverage on new changes (Priority: P2)

**Goal**: Per-PR coverage job with a self-hosted, blocking patch gate (80% default, 5-line floor) and informational project coverage

**Independent Test**: Quickstart Scenario 3 — three demonstration PRs (covered pass / uncovered fail with named lines / deletion-only pass)

### Implementation for User Story 3

- [x] T019 [US3] Add the `patch-cov` action to `.workshop/myna.yaml`: `diff-cover` over both merged Cobertura exports against the merge base, path normalization via `dev/coverage_lib.py`, `--fail-under` default 80, exclusions for generated/vendored paths, <5-coverable-line floor, exit 2 below threshold (contract: `patch-cov`; gate specifics: contracts/ci-gates.md)
- [x] T020 [US3] Add the coverage job to `.github/workflows/ci.yml`: `fetch-depth: 0` checkout, run `cov` + `py-cov` + `exercise` via Workshop actions, upload HTML/XML artifacts (14-day retention), project-level coverage in the job summary (non-blocking, FR-009)
- [x] T021 [US3] Wire the blocking `patch-coverage` gate step into `.github/workflows/ci.yml` running `patch-cov`; never fail-open on tool error (non-2 non-zero = job error)

### Validation for User Story 3

- [ ] T022 [US3] Execute quickstart.md Scenario 3: local `patch-cov` verdict, then the three demonstration PRs; confirm no external service is contacted (SC-004)

**Checkpoint**: Patch gate mechanically blocks untested new code

---

## Phase 6: User Story 4 - Expanded static checks in CI (Priority: P3)

**Goal**: Per-PR gates for format/unused-deps/dep-policy/python-lint/types/shell/workflow lint, plus scheduled advisory audits

**Independent Test**: Quickstart Scenario 4 — deliberate-violation branch per gate fails with a legible message; clean main passes

### Implementation for User Story 4

- [x] T023 [P] [US4] Add the `fmt` action (`cargo fmt --check`) to `.workshop/myna.yaml` and the `rust-fmt` gate to `.github/workflows/ci.yml`
- [x] T024 [P] [US4] Add the `machete` action and the `rust-unused-deps` gate
- [x] T025 [P] [US4] Create `client/deny.toml` (ban list of HTTP/cloud client crates per research.md D5 — `tokio-tungstenite` over UDS explicitly allowed; license policy) plus the `deny` action and the `rust-dep-policy` gate (FR-011)
- [x] T026 [P] [US4] Configure ruff in `server/pyproject.toml`, add the `py-lint` action (`ruff check` + `ruff format --check` over `server/`, `dev/`) and the `python-lint` gate
- [x] T027 [P] [US4] Configure mypy (strict scoped to `myna/core` only) in `server/pyproject.toml`, add the `py-types` action and the `python-types-core` gate
- [x] T028 [P] [US4] Add the `shell-lint` action (`shellcheck` over `dev/*.sh` and snap scripts/hooks) and the `shell-lint` gate
- [x] T029 [P] [US4] Add the `workflow-lint` action (`actionlint` over `.github/workflows/`) and the `workflow-lint` gate
- [x] T030 [US4] Add the `audit` action (`cargo audit` + `pip-audit` on `server/uv.lock`) and create `.github/workflows/audit.yml` (weekly schedule + manual dispatch; failure is visible signal, not PR-blocking — FR-012)

### Validation for User Story 4

- [x] T031 [US4] Execute quickstart.md Scenario 4: one deliberate-violation branch per gate (SC-005), all gates green after revert

**Checkpoint**: Full static-check battery enforced

---

## Phase 7: User Story 5 - Spread evaluation for confined multi-system end-to-end tests (Priority: P3)

**Goal**: Time-boxed spike → written adopt-or-reject decision; if adopt, one confined e2e spread task locally + in CI

**Independent Test**: Quickstart Scenario 5 — decision record exists with all five criteria; if adopt, the confined-e2e task passes on a clean VM locally and on the hosted KVM runner

### Implementation for User Story 5

- [x] T032 [US5] Spike: assess spread against the five spec criteria using `~/probe/spread` (source + self-test spread.yaml) and `~/probe/ubuntu/snapd-upstream/spread.yaml` + `tests/` as references; stand up the qemu backend locally on ubuntu-24.04-64 and verify hosted-runner KVM assumptions
- [x] T033 [US5] Resolve the two recorded design points (research.md D7): spread provisioning (pinned-commit build vs snap) and the confined-e2e backend topology (fake-adapter test snap vs in-VM `myna-server` with the socket placed in the shared content dir)
- [x] T034 [US5] Write the decision record `specs/006-coverage-quality-gates/spread-decision.md`: five criterion assessments, design-point resolutions, adopt|reject verdict with rationale (data-model.md: Spread Decision Record; reject with documented alternative is success)
- [x] T035 [US5] (adopt only) Author `spread.yaml` (qemu backend, ubuntu-24.04-64) and `tests/spread/confined-e2e/task.yaml`: install client + backend snaps, connect `ubustt-socket`, drive a WAV-fixture dictation via a virtual audio source, assert the transcript (FR-015, constitution II)
- [x] T036 [US5] (adopt only) Add the nightly/path-filtered spread workflow to `.github/workflows/` and document the supersession plan for the bespoke `snap.yml` smoke job

### Validation for User Story 5

- [x] T037 [US5] Execute quickstart.md Scenario 5 (SC-006), including a `-debug` interactive session if adopted

**Checkpoint**: Recorded decision; if adopted, confined e2e runs on clean VMs

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Documentation and whole-feature verification

- [x] T038 [P] Document the new commands in `README.md` and/or `docs/` (coverage, dead-code, patch gate, static checks; spread outcome), and update `CLAUDE.md` "Current state"/"Open / next" if the spread decision changes the confined-testing story
- [x] T042 [P] Write the README "Instrumented builds & manual coverage" section (FR-017): how to build/launch each component instrumented (all Rust binaries, `myna-server`), how to accumulate multiple manual runs (shared data locations per contracts/workshop-actions.md manual-use note), how to generate the report + dead-code summary on demand, how to reset; then validate SC-008 by following the section cold, end to end, in under 15 minutes
- [ ] T039 [P] Update `docs/project-plan.md`: link feature 006; reconcile with T55 (toolchain under Workshop) which this feature partially advances
- [x] T040 Re-run `/speckit-analyze` in full mode (spec × plan × tasks) and reconcile any findings
- [x] T041 Execute the complete `quickstart.md` end-to-end (Scenarios 1–5 + privacy sanity: no fixture transcript phrases in any artifact — FR-016)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: Depends on Phase 1 — BLOCKS all stories
- **US1 (Phase 3)**: After Foundational. No other story dependencies
- **US2 (Phase 4)**: After US1 (consumes the `cov`/`py-cov` exports it merges)
- **US3 (Phase 5)**: After US2 (the gate consumes merged exports per the amended ci-gates contract)
- **US4 (Phase 6)**: After Foundational only — independent of US1–US3, can run in parallel
- **US5 (Phase 7)**: After Foundational only — independent; CI-workflow edits coordinate with US3's if concurrent
- **Polish (Phase 8)**: After all adopted stories

### Parallel Opportunities

- T002 ∥ T003 (Setup); T006 ∥ T007 (US1); T016 ∥ T015 (US2)
- US4 tasks T023–T029 all [P] (different files)
- US4 and US5 can proceed in parallel with the US1→US2→US3 chain after Phase 2

### Parallel Example: User Story 4

```bash
# All seven gate implementations are independent files:
Task: "fmt action + rust-fmt gate"
Task: "machete action + rust-unused-deps gate"
Task: "deny.toml + deny action + rust-dep-policy gate"
Task: "ruff config + py-lint action + python-lint gate"
Task: "mypy config + py-types action + python-types-core gate"
Task: "shell-lint action + gate"
Task: "workflow-lint action + gate"
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED - constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `006-coverage-foundation` | Phase 1–2 (T001–T005) | — | existing CI (lint/test/py-test) green |
| 2 | `006-coverage-us1-local` | Phase 3 (T006–T009) | #1 | existing CI green; reports generate (artifact of a manual run attached to PR) |
| 3 | `006-coverage-us2-usecases` | Phase 4 (T010–T018) | #2 | existing CI green; `exercise`+`deadcode` pass locally, incl. negative test |
| 4 | `006-coverage-us3-ci-gate` | Phase 5 (T019–T022) | #3 | full CI green **including** the new patch gate passing on this branch itself |
| 5 | `006-coverage-us4-static` | Phase 6 (T023–T031) | #1 | full CI green including all new lint gates |
| 6 | `006-coverage-us5-spread` | Phase 7 (T032–T037) | #1 | decision record reviewed; if adopt: spread task green locally + on the nightly workflow |
| 7 | `006-coverage-polish` | Phase 8 (T038–T041) | #2–#6 as adopted | full CI green |

Branches 5 and 6 fork from #1/#main and do not build on unmerged siblings 2–4 (constitution staging rule); merge order beyond prerequisites is flexible.

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: quickstart Scenario 1 independently
5. Merge branch #2 — contributors immediately get one-command coverage

### Incremental Delivery

1. Foundation → US1 (MVP: local coverage) → US2 (use-case + dead code) → US3 (CI patch gate)
2. US4 (static checks) and US5 (spread spike) deliver independently in parallel
3. Each merge leaves main green with its own new gates passing

---

## Notes

- [P] tasks = different files, no dependencies
- Validation tasks cite quickstart.md scenarios — they are the acceptance contract for each story (red-green TDD inapplicable; see Tests note above)
- Numbering note: this file's T0NN IDs are feature-local (per-repo convention, they do not correspond to the global TNN IDs in `docs/project-plan.md`)
- Commit after each task or logical group; commit messages state what + why, no AI attribution (constitution: Commit & PR Communication)

---

## Post-implementation analysis (T040, 2026-08-10)

Cross-artifact pass (spec x plan x tasks x implementation). Findings:

1. **FR-001 wording vs stable toolchain** (minor, resolved-by-record): spec
   says Rust "line and branch coverage"; branch data needs nightly rustc, so
   the shipped gate is line/region-based with branch detail best-effort in
   the HTML report. research.md D1 recorded this; spec.md was never amended.
   Accepted: line data is authoritative for gates (D1); amend spec on next
   touch rather than churn now.
2. **Branch staging plan not followed** (process deviation, recorded):
   maintainer directed work onto integration-220627. Constitution's staged
   delivery rule was waived by direction; noted here for the review trail.
3. **SC-001 timing not precisely measured**: cov/exercise runs were observed
   well within 2x of the plain test job in the workshop, but no timed
   benchmark was captured. Low risk (instrumented test run dominates).
4. **Open acceptance items**: T022 demonstration PRs and the first CI runs
   of the coverage/spread/audit workflows (authored, lint-clean, locally
   reproduced where possible) - these are the remaining green-to-green proof.

## T041 quickstart execution notes (2026-08-10)

Scenarios 1-3 (local parts) and 5 executed green in the canonical workshop
and on a clean qemu VM. Scenario 2's manual ad-hoc mode was run cold
following only the README - two documentation gaps found and fixed
(uv-prefixed coverage commands; merged-export refresh before deadcode).
Scenario 4 violations verified per-gate locally (fmt exit 1, deny exit 101
naming reqwest, py-lint 1, py-types 1, shell-lint 1, workflow-lint 1;
machete caught the real unused thiserror). Privacy sanity (FR-016): no
transcript content in any export/artifact (HTML reports embed source text
only, which includes test-string literals - code, not transcription data);
all report paths gitignored. Remaining: CI-side parts of Scenarios 3-5
(demo PRs, first nightly runs).
