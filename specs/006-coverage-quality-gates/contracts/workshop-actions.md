# Contract: Named Workshop Actions

Every command this feature adds is a named action in `.workshop/myna.yaml`,
runnable identically by contributors (`workshop run myna <action>`) and CI
(constitution IV, FR-013). Conventions: exit 0 = success with reports at the
documented paths; non-zero = failure with a legible message naming the cause;
all report paths are repo-relative and gitignored.

## `cov` — Rust workspace coverage (FR-001)

- **Runs**: `cargo llvm-cov` over `client/` full workspace test suite.
  Honors existing gates (`MYNA_PIPEWIRE_TESTS`, desktop-session gates) — closed
  gates skip cleanly, open gates contribute hits (FR-003).
- **Outputs**: `client/target/coverage/html/index.html` (browsable),
  `client/target/coverage/rust.lcov`,
  `client/target/coverage/rust-tests.cobertura.xml`.
- **Exit**: non-zero if any test fails or report generation fails.

## `py-cov` — Python suite coverage (FR-002)

- **Runs**: `uv run pytest --cov=myna --cov-branch --cov-context=test` in `server/`.
- **Outputs**: `server/htmlcov/index.html` (with per-line contexts),
  terminal missing-lines summary, `server/coverage-tests.cobertura.xml`.
- **Exit**: non-zero if any test fails or report generation fails.

## `exercise` — use-case runs under instrumentation (FR-004)

- **Runs** `dev/exercise.sh`: fake-adapter `myna-server` + `myna-dictate`
  sessions in internal and IE115 dialects (recorded/WAV input), plus the
  `myna-desktop --dbus` publisher path against a stub consumer; live-mic
  scenario only when explicitly gated open. Both toolchains instrumented
  (coverage.py parallel mode + contexts; llvm-cov run).
- **Outputs**: raw coverage data merged into the same locations as `cov` /
  `py-cov` (merged = tests + use-cases), plus merged exports
  `client/target/coverage/rust-merged.cobertura.xml`,
  `server/coverage-merged.cobertura.xml`.
- **Exit**: non-zero if any scripted use-case fails its assertion (e.g.,
  expected transcript event) or if an expected raw data file is missing after
  a scheduled run (fail-loud merge, FR-005).

### Manual ad-hoc use (FR-017)

The `cov` / `py-cov` / `exercise` actions are conveniences over documented
primitives, not the only entry point. The README's instrumented-build section
documents the underlying commands directly — instrument and launch any binary
by hand, accumulate arbitrarily many manual runs, generate the report on
demand, and reset accumulated data — so maintainers can cover ad-hoc manual
testing without the scripted exercise. The actions and the manual workflow
MUST write to the same data locations so any mix of scripted and manual runs
merges into one report.

## `deadcode` — populations + dead-code report (FR-005, FR-006)

- **Runs** `dev/coverage_populations.py` over the tests-only and merged
  Cobertura exports; appends static findings (`cargo machete`, vulture with
  `dev/vulture_allowlist.py`, `ruff --select F401,F841`).
- **Outputs**: `client/target/coverage/populations.md` — per-language,
  per-component tables of test-covered / use-case-only / never-executed, and
  the dead-code section; `client/target/coverage/populations.json`
  (machine-readable).
- **Exit**: non-zero if required exports are missing; the report itself is
  advisory (never fails on findings at introduction).

## `patch-cov` — self-hosted patch gate (FR-008; CI-oriented, locally runnable)

- **Runs** `diff-cover` against the merge base with the Cobertura exports,
  after path normalization to repo-root-relative; `--fail-under` configurable
  (default 80). Zero coverable changed lines → pass. Floor: diffs with fewer
  than 5 coverable changed lines pass unconditionally (rounding noise guard).
- **Outputs**: terminal verdict naming uncovered changed lines on failure;
  `client/target/coverage/patch-coverage.html` artifact.
- **Exit**: 0 = pass; 2 = below threshold (fail); other non-zero = tool error
  (never fail-open).

## Lint additions (FR-010, FR-011)

Extend existing `lint` action or add sibling actions — same contract as the
current `lint` (exit non-zero naming violations):

- `fmt`: `cargo fmt --check`
- `deny`: `cargo deny check bans licenses` (ban list per research D5)
- `machete`: `cargo machete` (unused workspace deps)
- `py-lint`: `ruff check` + `ruff format --check` (server/, dev/)
- `py-types`: `mypy` scoped to `myna/core` (strict) — harness tier elsewhere
- `shell-lint`: `shellcheck` over `dev/*.sh`, snap hooks/scripts
- `workflow-lint`: `actionlint` over `.github/workflows/`

## Audits (FR-012; scheduled, not per-PR)

- `audit`: `cargo audit` (RustSec) + `pip-audit` over `server/uv.lock`.
- **Exit**: non-zero when advisories found; consumed by a weekly scheduled
  workflow whose failure is visible signal, not a PR block.
