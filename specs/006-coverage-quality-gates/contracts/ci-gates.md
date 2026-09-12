# Contract: CI Quality Gates

Gate contract fields per data-model.md (name, scope, blocking, inputs, failure
output). All per-PR gates run in `.github/workflows/ci.yml` via the Workshop
actions in `contracts/workshop-actions.md` — the action is the single source of
the command; the workflow only orchestrates triggers, checkout depth, and
artifacts.

## Per-PR gates (blocking)

| Gate | Workshop action | Failure output MUST name |
|------|-----------------|--------------------------|
| `rust-lint` (existing) | `lint` | violating file:line, lint id |
| `rust-test` (existing) | `test` | failing test name |
| `py-test` (existing) | `py-test` | failing test name |
| `rust-fmt` | `fmt` | misformatted file |
| `rust-unused-deps` | `machete` | unused dependency + crate |
| `rust-dep-policy` | `deny` | banned dependency + the crate that introduced it |
| `python-lint` | `py-lint` | file:line, rule id |
| `python-types-core` | `py-types` | file:line, mypy error |
| `shell-lint` | `shell-lint` | script:line, shellcheck id |
| `workflow-lint` | `workflow-lint` | workflow:line, actionlint error |
| `patch-coverage` | `patch-cov` | each uncovered changed line (file:line) |

### `patch-coverage` gate specifics (FR-008, FR-009)

- **Trigger**: pull_request. Checkout `fetch-depth: 0` (needs merge base).
- **Inputs**: `client/target/coverage/rust-merged.cobertura.xml`,
  `server/coverage-merged.cobertura.xml`. The CI coverage job runs `cov`,
  `py-cov`, AND `exercise` (the fake-adapter/WAV/publisher scenarios are
  hermetic in the Workshop environment; gated live-capture skips), so the gate
  always consumes merged exports — never tests-only.
- **Pass**: covered-changed-lines / coverable-changed-lines ≥ threshold
  (default 80), zero coverable changed lines, or fewer than 5 coverable
  changed lines (rounding-noise floor).
- **Fail (exit 2)**: below threshold; log MUST list every uncovered changed
  line. Never fail-open on tool error.
- **Exclusions**: generated/vendored paths, `specs/`, `docs/`, snapshots —
  declared as patterns in one place (the `patch-cov` action).
- **Project-level coverage**: reported in the job summary only; non-blocking
  until ratified separately (FR-009).

## Per-PR gates (non-blocking, informational)

| Gate | Output |
|------|--------|
| `coverage-rust` | HTML + Cobertura artifacts uploaded on every PR |
| `coverage-python` | HTML + Cobertura artifacts uploaded on every PR |

Artifacts retained per workflow defaults (14 days, matching snap.yml).

## Scheduled gates (weekly; signal, not PR-blocking)

| Gate | Action | On failure |
|------|--------|------------|
| `rust-audit` | `audit` (cargo audit) | failing scheduled run + (optionally) auto-filed issue |
| `python-audit` | `audit` (pip-audit) | same |

## Trigger hygiene

- Coverage + new lint gates: every pull_request and pushes to main (same as
  ci.yml today).
- `rust-dep-policy`/`python-lint` may add path filters later if runtime cost
  demands; not at introduction (keep signal complete while cheap).
- `snap.yml` unchanged by this feature; if the spread record decides `adopt`,
  a follow-up change moves confined e2e to a nightly/path-filtered spread
  workflow and supersedes the bespoke smoke job.
