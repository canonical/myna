# Preface

Read this before reporting any code change as done. It defines what done means in this repository: which gates must be green, how test strength is judged, and how tests are written.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

The Makefile is the one definition of every gate CI enforces. The top-level targets are CI's jobs, one each: `check` (static gates), `test` (every blocking suite), `coverage` (coverage reports, the dead-code digest and the patch-coverage gate), `spread` (confined end-to-end in a VM) and `snaps` (package every snap). `make preflight` runs the first three, which is exactly what blocks a merge. Everything under those gates runs inside the Workshop environment, so green locally is green in CI.

CodeQL (`.github/workflows/codeql.yml`) runs on every pull request as well, advisory: its findings appear in the repository's Security tab and on the PR, and each is triaged as a decision, never allow-listed to unblock a merge.

Component-scoped targets are `<verb>-<component>`: `fmt`, `lint`, `test`, `cov`, `mutate` and `build` over `client`, `server` and `extension`, with a third part for a sub-suite (`test-client-gated`). `make help` lists them grouped by purpose.

# Important

- After every code change, run `make check` and `make test-<component>` for each component touched. Both must be green before the change is reported done; report a red gate as red, with its output. Run `make preflight` before proposing a merge.
- Fix the code, not the gate. Never silence a lint, skip a test, lower a threshold or widen an allowlist to get green; if a gate is wrong, change it in its own commit with the reason in the commit body.
- Coverage is judged on the patch. `make coverage` runs the reports and ends with `cov-patch`, the blocking gate: 80% of the changed coverable lines, five-line floor, measured against `COV_BASE` (default `origin/main`). Run it whenever a change adds or moves logic, and read the dead-code digest it prints: never-executed code is deleted, not tested (see the digest before writing tests for coverage).
- Red then green. Write the test first and run it to see it fail for the reason expected; then the minimal implementation that makes it pass; then refactor with the suite green. A test that was never seen failing proves nothing. A bug fix starts with the end-to-end reproduction as the user hits it, turned into a failing test.
- Mutation testing when the suite's strength is in doubt: a bug got past it, a new state machine or protocol path, a threshold or gate. Scope it to the code just written, never the whole tree (hours): `make mutate-client MUTATE='-p <crate> -f <file>'` (cargo-mutants args) or `make mutate-server MUTATE='<module glob>*'` (mutmut mutant names). Every surviving mutant is a missing assertion or dead code; resolve each one before calling the suite done.
- Generated files are regenerated, not edited: translation templates with `make i18n` (checked by `make check`), corpora and coverage exports by their targets.
