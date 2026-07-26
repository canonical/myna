# Phase 1 Data Model: Coverage, Dead-Code Visibility, and Quality Gates

Entities here are report/gate data structures, not runtime persistence. All are
derived artifacts (files), never committed source of truth.

## Coverage Data Set

One per (language, run-kind): `language ∈ {rust, python}`,
`run_kind ∈ {tests, usecase:<name>}`.

- **Fields**: line hits per file (line → hit count), branch hits where the
  toolchain reports them (coverage.py: branch arcs; llvm-cov: region/branch
  data), provenance (which run produced it; Python additionally carries
  coverage.py contexts per line).
- **Forms**: raw (`.coverage*` parallel files, `*.profraw`) → merged internal →
  exports: HTML (browsable; Python with `--show-contexts`), Cobertura XML
  (machine-readable, input to populations + patch gate), lcov (Rust;
  future-proofing for hosted-service compatibility per spec assumption).
- **Validation rules**: a merged export MUST be produced from ≥1 expected raw
  input per run-kind that was scheduled; missing expected input is a merge
  failure, not an empty report (FR-005). Gated-but-skipped suites contribute
  zero hits but MUST appear as uncovered lines in their files, never as absent
  files (edge case: distinguishable "not run" vs "no code").

## Coverage Population

Derived per language by set arithmetic over two merged exports
(tests-only, tests+use-cases) at line granularity, reported at file and
region granularity.

- **Classes**: `test_covered` (hits > 0 in tests-only), `usecase_only`
  (hits = 0 in tests-only, > 0 in merged), `never_executed` (0 in both).
- **Validation rules**: every coverable line belongs to exactly one class;
  `test_covered ∪ usecase_only ∪ never_executed` = all coverable lines;
  `usecase_only` lines flag integration-test gaps, `never_executed` feeds the
  dead-code report.

## Dead-Code Report

- **Fields**: dynamic entries (never_executed regions, per file, per language),
  static entries (`unused_dependency` from cargo-machete; `unused_symbol` from
  vulture ≥ confidence 80; `unused_import/name` from ruff F401/F841), minus
  allow-listed symbols (`dev/vulture_allowlist.py`; Rust dynamic dispatches are
  trait-impl-based and already visible to the compiler).
- **Validation rules**: zero false positives for allow-listed entry points
  (SC-003); report is grouped by component so a maintainer can act per crate/
  package; advisory (not a merge gate) at introduction.

## Patch-Coverage Verdict

One per pull request.

- **Fields**: base ref, changed coverable lines (from git diff ∩ coverable
  lines), covered-changed lines, percentage, threshold (default 80,
  configurable), verdict `pass|fail`, list of uncovered changed lines.
- **State transitions**: `computed → pass` (percentage ≥ threshold, or zero
  coverable changed lines — deletions/config-only) | `computed → fail`
  (percentage < threshold, names uncovered lines). No other terminal states;
  tool failure ≠ fail-open (job errors, gate does not silently pass).
- **Validation rules**: generated/vendored paths excluded by pattern;
  percentage computed over coverable changed lines only (FR-008).

## Quality Gate

Named CI check with explicit contract (see contracts/ci-gates.md).

- **Fields**: name, scope (per-PR | scheduled), blocking (bool), inputs,
  failure output contract (what it must name on failure).
- **Instances added by this feature**: `coverage-rust` (reporting),
  `coverage-python` (reporting), `patch-coverage` (blocking, per-PR),
  `rust-fmt`, `rust-unused-deps`, `rust-dep-policy` (blocking, per-PR),
  `python-lint`, `python-types-core` (blocking, per-PR), `shell-lint`,
  `workflow-lint` (blocking, per-PR), `rust-audit`, `python-audit`
  (non-blocking signal, scheduled).

## Spread Decision Record

- **Fields**: date, the five criterion assessments (clean-system lifecycle,
  multi-system matrix, hosted-runner CI feasibility, virtual-audio support,
  debug ergonomics), backend-provisioning choice, confined-e2e backend design
  (test-snap vs in-VM fake server), verdict `adopt|reject`, rationale,
  follow-ups (if adopt: suite location, CI workflow, snap.yml smoke
  supersession plan; if reject: chosen alternative).
- **Validation rules**: reject is a successful terminal state if rationale +
  alternative are recorded (spec assumption); record lives under
  `specs/006-coverage-quality-gates/`.
