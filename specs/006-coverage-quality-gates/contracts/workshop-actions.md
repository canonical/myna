# Contract: Named Workshop Actions

Every command this feature adds is a named action in `.workshop/myna.yaml`,
runnable identically by contributors (`workshop run myna <action>`) and CI
(constitution IV, FR-013). Conventions: exit 0 = success with reports at the
documented paths; non-zero = failure with a legible message naming the cause;
all report paths are repo-relative and gitignored.

## `cov` — Rust workspace coverage (FR-001)

- **Runs**: `cargo llvm-cov` over `client/` full workspace test suite, then the
  env-gated hardware suites through `dev/gated-tests.sh`, and reports the two
  merged.
  Honors existing gates (`MYNA_PIPEWIRE_TESTS`, desktop-session gates) — closed
  gates skip cleanly, open gates contribute hits (FR-003). The gates are no
  longer left closed by default: `gated-tests.sh` stands the services up (a
  private PipeWire graph with a virtual mic, a private D-Bus session with its
  own IBus daemon) and opens each gate only once its service answers, so a
  runner without one falls back to the clean skip rather than a failure.
- **Outputs**: `client/target/coverage/html/index.html` (browsable),
  `client/target/coverage/rust.lcov`,
  `client/target/coverage/rust-tests.cobertura.xml`.
- **Exit**: non-zero if any test fails or report generation fails.

## `test-gated` (env-gated hardware suites)

- **Runs**: `dev/gated-tests.sh cargo test` over `pipewire_hw`, `ibus_hw` and
  `dbus_hw`. The same suites `test` runs hermetically, with their services
  present. `ibus_hw` drives the session-global input engine, so it runs
  serially and inside a private session bus, never against the caller's
  desktop.
- **Outputs**: test results only; `cov` is what turns these runs into coverage.
- **Exit**: non-zero if any test fails. A service that does not come up leaves
  its gate closed and its suite skipping, which is a pass.

## `py-cov` — Python suite coverage (FR-002)

- **Runs**: `uv run pytest --cov=myna --cov-branch --cov-context=test` in `server/`.
- **Outputs**: `server/htmlcov/index.html` (with per-line contexts),
  terminal missing-lines summary, `server/coverage-tests.cobertura.xml`.
- **Exit**: non-zero if any test fails or report generation fails.

## `exercise` — use-case runs under instrumentation (FR-004)

- **Runs** `dev/exercise.sh`, which re-executes itself under
  `dev/gated-tests.sh` so the services its scenarios need are present rather
  than waited for: fake-adapter `myna-server` + `myna-dictate` sessions in
  internal and IE115 dialects and from a corpus manifest (recorded/WAV input);
  the packaged `myna-desktop` daemon against the virtual capture source, a
  private session bus and a private IBus daemon; capture from that source
  through the native backend; and the one-shot entry points (`--help`,
  rejected argument combinations, `--toggle`, `--install-shortcut`). Both
  toolchains instrumented (coverage.py parallel mode + contexts; llvm-cov run).
  `MYNA_LIVE_TESTS=1` aims the capture scenario at the machine's default
  device instead of the virtual one; `MYNA_EXERCISE_NO_GATES=1` skips the
  re-exec and leaves the service-dependent scenarios skipping.
- **Clean exits are load-bearing**: an instrumented binary writes its `.profraw`
  from an `atexit` handler, so every scenario quits its binary through EOF on
  stdin. A scenario that killed one with a signal would score as zero coverage
  while still passing.
- **Why the wrapper**: the desktop scenario used to gate on `MYNA_PIPEWIRE_TESTS`
  being exported by hand, which nothing did, so it had never run and every
  `myna-desktop` entry point read as dead code. `cov` already stood the same
  services up through `gated-tests.sh`; `exercise` now does too.
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
- **Outputs**: a digest on **stdout** — per-language population totals with
  percentages, per-component debt ranked by never-executed lines, the
  never-executed hot-spot files, the never-entered function count, and a
  one-line verdict per static tool. The same digest as markdown at
  `client/target/coverage/populations-summary.md` (CI appends it to the job
  summary, so the debt number is visible on every run without downloading an
  artifact); the full report — digest plus every never-entered function by
  name, never-executed line spans per file, and raw static output — at
  `client/target/coverage/populations.md`; `client/target/coverage/populations.json`
  (machine-readable, including totals and the never-entered functions).
- **Staleness**: exports naming files the tree no longer has are excluded, and
  both that and "sources changed since the exports were written" are reported
  as warnings at the top of the digest — a stale report otherwise describes
  debt that is already gone.
- **Options**: `--top N` (hot-spot files in the digest, default 15),
  `--no-statics` (skip the static tools).
- **Exit**: non-zero if required exports are missing; the report itself is
  advisory (never fails on findings at introduction). A static tool that is
  not installed is reported as "not run", never as clean.

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
