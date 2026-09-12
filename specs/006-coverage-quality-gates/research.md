# Phase 0 Research: Coverage, Dead-Code Visibility, and Quality Gates

All Technical Context items were resolvable from repo inspection and ecosystem
knowledge; no open NEEDS CLARIFICATION remains. Decisions below.

## D1. Rust coverage instrumentation

- **Decision**: `cargo llvm-cov` (LLVM source-based coverage) with
  `llvm-tools-preview`; exports `--html`, `--lcov`, `--cobertura`.
- **Rationale**: Most accurate line/branch data for Rust; first-class workspace
  support; no rebuild hacks; emits the Cobertura XML the self-hosted patch gate
  consumes. Use-case runs use `cargo llvm-cov run --bin myna-dictate|--bin
  myna-desktop -- …` which instruments the shipped binaries, then
  `cargo llvm-cov report` merges their `.profraw` with the test run.
- **Alternatives considered**: cargo-tarpaulin (binary-level, historically
  flaky with workspaces/proc-macros; lcov only); grcov + manual
  `-Cinstrument-coverage` (same engine as llvm-cov but we maintain the glue).
- **Known caveat**: llvm-cov branch data is region-based and its Cobertura
  branch export is best-effort; the patch gate and populations classify on
  **line** data (authoritative), with branch detail treated as informational
  on the Rust side (spec FR-001 branch requirement is met by the HTML report,
  not the export).

## D2. Python coverage and use-case/subprocess capture

- **Decision**: coverage.py via pytest-cov for tests (`--cov=myna --cov-branch
  --cov-context=test`); use-case runs via `coverage run --parallel-mode
  --context=usecase:<name> -m myna.server.cli …`, with
  `COVERAGE_PROCESS_START` for any spawned subprocess; `coverage combine`
  merges; HTML with `--show-contexts`.
- **Rationale**: `server/pyproject.toml` already configures
  `[tool.coverage.run] source=["myna"], branch=true` and ships pytest-cov in
  the dev group — this completes existing intent. Contexts give per-line
  "covered by which test/use-case" attribution for free.
- **Alternatives considered**: slipcover (faster, but young and no context
  support); coverage without contexts (loses the test-vs-usecase distinction
  the spec's populations need on the Python side).

## D3. Self-hosted patch gate (spec decision: Option A)

- **Decision**: `diff-cover` run in CI against the PR base (`origin/main…HEAD`
  diff, checkout with `fetch-depth: 0`), consuming Cobertura XML from both
  toolchains, `--fail-under=80` (configurable), output as job log + artifact.
- **Rationale**: Zero data leaves CI (constitution V / spec decision);
  diff-cover is the standard Cobertura-diff tool and needs no service.
- **Known wrinkle**: diff-cover matches XML file paths against
  repo-root-relative git diff paths. Rust llvm-cov Cobertura paths are
  workspace-relative (`client/…` prefix handling) and Python paths are
  `server/…`-relative; a small normalization step (rewrite `<source>`/filename
  prefixes in `dev/coverage_populations.py` or a dedicated filter) keeps both
  aligned. Deletion-only diffs yield zero coverable lines → diff-cover reports
  100% — satisfies the no-false-failure requirement; generated/vendored paths
  excluded via `--exclude` patterns.
- **Alternatives considered**: Codecov/Coveralls (rejected by spec decision —
  external service); pycobertura diff (Python-only, weaker CLI gating).

## D4. Coverage populations and dead-code report

- **Decision**: `dev/coverage_populations.py` parses the tests-only and merged
  Cobertura exports per language and classifies every line/region:
  test-covered, use-case-only, never-executed. Dead-code report = never-executed
  ∪ static findings: `cargo machete` (unused Rust deps), existing `dead_code`
  lint (already fatal under `clippy -D warnings`), `vulture
  --min-confidence 80` with `dev/vulture_allowlist.py`, `ruff --select F401,F841`.
- **Rationale**: Cobertura is the one format both toolchains already emit;
  region classification is set arithmetic on line hits — cheap and auditable.
  Vulture needs an allow-list for adapters loaded by name and pytest fixtures.
- **Alternatives considered**: coverage.py-only JSON + custom Rust parser (two
  parsers instead of one); cargo-udeps (nightly-only — machete covers the 95%
  case on stable).

## D5. Static checks beyond coverage

- **Decision**: per-PR — `cargo fmt --check`, `cargo machete`, `cargo deny check
  bans licenses` (ban list: HTTP/cloud client crates — e.g. reqwest,
  hyper-client features, aws/azure/gcp SDKs — applied to the shipped client
  crates; note `tokio-tungstenite` over UDS is the sanctioned local transport
  and is NOT banned), `ruff check` + `ruff format --check`, `mypy` scoped
  strict on `myna/core` only, `shellcheck dev/*.sh` + snap hooks, `actionlint`.
  Scheduled weekly — `cargo audit`, `pip-audit` on `uv.lock`.
- **Rationale**: FR-010/FR-011/FR-012. Scoped mypy keeps the load-bearing
  contract honest without boiling the harness ocean. Scheduled audits avoid
  blocking unrelated PRs on newly-published advisories.
- **Alternatives considered**: per-PR audits (noisy, blocks merges on embargoed
  CVE churn); typos/docs lint (nice-to-have, deferred — docs-lint.yml exists).

## D6. Workshop integration

- **Decision**: new named actions in `.workshop/myna.yaml` — `cov` (Rust),
  `py-cov`, `exercise`, `deadcode`, `patch-cov`, and lint additions; tool
  provisioning rides the existing SDK pattern (cargo-binstall/cargo-install in
  the rust SDK; `uv` dev-group additions for Python tools).
- **Rationale**: Constitution IV — CI already calls Workshop actions
  (`workshop run myna lint|test|py-test`); extending the same file keeps "green
  in CI = green locally" true for every new gate.

## D7. Spread evaluation plan (US5)

- **Decision**: time-boxed spike against the five spec criteria, using
  `~/probe/spread` (source) and `~/probe/ubuntu/snapd-upstream/spread.yaml` +
  `tests/` as references. Provisioning: build spread from a pinned upstream
  commit (snapd CI pattern) or the spread snap if channel-verified; record the
  choice in the decision record. First task if adopted: qemu backend,
  `ubuntu-24.04-64`, KVM on `ubuntu-latest` runners (snapd relies on this);
  install `myna` snap + backend, connect `ubustt-socket`, drive a WAV-file
  dictation, assert transcript.
- **Open design point for the spike**: backend for the confined e2e — either
  (a) a minimal fake-adapter test snap providing the content slot, or
  (b) `myna-server --adapter fake` run directly in the VM with the session
  socket placed into the shared content directory. (a) is the honest confined
  topology, (b) is faster to stand up; the spike decides. Virtual audio: a
  PipeWire null/virtual source in the guest satisfies constitution II; desktop
  session (hotkey/IBus/indicator) explicitly out of spike scope per spec.
- **Alternatives considered**: extend the bespoke `snap.yml` smoke script
  (single system, no lifecycle/debug tooling — spread supersedes if adopted);
  LXD backend (faster, but confined-snap-in-container quirks; qemu is the
  reference backend).

## Resolved items from Technical Context

No NEEDS CLARIFICATION entries remained after spec clarification (patch-gate
mechanism: self-hosted, decided 2026-07-26). All dependencies above have
stable releases and offline-capable installation paths compatible with the
Workshop environment.
