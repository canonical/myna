# Quickstart: Validating Coverage, Dead-Code Visibility, and Quality Gates

Runnable end-to-end validation once implemented. Commands are the Workshop
actions from `contracts/workshop-actions.md`; gate behavior per
`contracts/ci-gates.md`.

## Prerequisites

- Workshop environment launched (`workshop launch myna`), repo at `/project`.
- No new host mutations beyond Workshop itself (constitution IV) — all tools
  arrive via the Workshop SDK definitions.

## Scenario 1 — Local coverage, both languages (US1, SC-001)

```bash
workshop run myna cov       # Rust: full workspace suite, instrumented
workshop run myna py-cov    # Python: offline suite, branch + contexts
```

Expect: exit 0; open `client/target/coverage/html/index.html` and
`server/htmlcov/index.html`; per-line red/green with branch detail; Cobertura +
lcov exports exist. Python HTML shows "covered by which test" contexts.

## Scenario 2 — Use-case coverage and dead code (US2, SC-002, SC-003)

```bash
workshop run myna exercise  # fake-adapter dictation ×2 dialects, WAV, desktop
                            # publisher — all instrumented
workshop run myna deadcode  # merge + classify + statics
```

Expect: exit 0; `client/target/coverage/populations.md` classifies every region as
test-covered / use-case-only / never-executed. Spot-checks: the fake-adapter
WS session path shows test-covered; a desktop-session-gated path shows its true
population (use-case-only or never-executed with the gate closed — not an
error). Dead-code section contains no allow-listed dynamic entry points
(adapter-by-name loaders, pytest fixtures).

Negative test: delete one expected raw data file and re-run `exercise` → the
merge MUST fail loudly naming the missing input (FR-005).

### Manual ad-hoc mode (FR-017, SC-008)

Following **only the README's instrumented-build section** (no
coverage-tooling knowledge assumed): clean the accumulated data, launch the
server and client binaries instrumented, poke at the app by hand (several
runs, several binaries), then run the documented report command. Expect: the
merged HTML report reflects exactly the manual session (lines you touched are
green, ones you didn't are red), repeated runs accumulated rather than
overwrote, and `deadcode` classifies the session's hits as use-case-covered.
Then clean again to reset for the next session.

## Scenario 3 — Patch gate (US3, SC-004)

Locally:

```bash
git fetch origin main
workshop run myna patch-cov   # diff-cover vs merge base, threshold 80
```

Expect: verdict printed; exit 0 on pass, 2 below threshold with each uncovered
changed line named.

In CI, open three demonstration PRs:

1. Adds a function **with** a test → `patch-coverage` passes.
2. Adds a function **without** a test → `patch-coverage` fails, log names the
   new uncovered lines; `coverage-rust`/`coverage-python` artifacts are
   downloadable from the run.
3. Deletion-only change → `patch-coverage` passes (zero coverable lines).

Also confirm: no external service is contacted (self-hosted gate); project-level
coverage appears in the job summary and does not block (FR-009).

## Scenario 4 — Static checks (US4, SC-005)

For each new gate, one deliberate-violation branch:

| Violation | Gate that must fail |
|---|---|
| unformatted Rust file | `rust-fmt` |
| dep added to a crate's Cargo.toml, never used | `rust-unused-deps` |
| `reqwest` added to `myna-desktop` | `rust-dep-policy` (offline invariant) |
| unused import in `server/` | `python-lint` |
| type error in `myna/core` | `python-types-core` |
| unquoted variable in `dev/*.sh` | `shell-lint` |
| invalid workflow key | `workflow-lint` |

With violations reverted, all pass on main. `audit` runs weekly; simulate via
manual workflow dispatch — advisories (if any) produce visible signal without
touching PRs.

## Scenario 5 — Spread evaluation (US5, SC-006)

1. Read the decision record at `specs/006-coverage-quality-gates/` — five
   criteria assessed, verdict adopt|reject with rationale.
2. If **adopt**: `spread qemu:ubuntu-24.04-64:tests/spread/confined-e2e` (or
   the recorded invocation) runs the confined client+backend snap dictation on
   a clean VM with virtual audio, asserting the transcript; re-run with the
   debug flag to land in an interactive VM shell; the nightly/path-filtered CI
   workflow runs the same task on the hosted KVM runner.
3. If **reject**: the record names the alternative and the bespoke `snap.yml`
   smoke job remains the confined gate.

## Privacy sanity (FR-016, constitution V)

After Scenarios 1–2: `git status` shows only gitignored report paths; reports
contain code-structure metadata only — grep any artifact for a known fixture
transcript phrase and expect no hits.
