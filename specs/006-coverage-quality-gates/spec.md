# Feature Specification: Coverage, Dead-Code Visibility, and Quality Gates

**Feature Branch**: `[006-coverage-quality-gates]`

**Created**: 2026-07-26

**Status**: Implemented (validation pending: fresh-Workshop actions, demo PRs, spread VM run)

**Input**: User description: "Plan how to run the Rust and Python tests with coverage, exercise the program using real use-cases, and then see the dead code; ensure CI measures coverage for new changes; add any other useful static checks; evaluate the spread integration test framework."

This feature is developer/CI tooling for the myna repo. Its "users" are myna
contributors and maintainers; its "product" is trustworthy signal about what the
test suites and real use-cases actually execute, enforced on every change.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - One-command local coverage for both languages (Priority: P1)

A contributor working in the canonical Workshop environment runs a single named
command per language and gets a browsable coverage report for the full Rust
workspace test suite and the full Python test suite, with branch coverage,
per-line hit/miss detail, and a machine-readable export. The same commands run
unchanged in CI, so "green in CI" and "green locally" stay the same statement
(constitution Principle IV).

**Why this priority**: Coverage is the foundation everything else in this
feature builds on — dead-code analysis, use-case gap analysis, and the CI patch
gate all consume these reports. Without it nothing else here can exist.

**Independent Test**: In a fresh Workshop environment, run the Rust coverage
command and the Python coverage command; each produces an HTML report that opens
in a browser and a machine-readable file, and the reported line totals match the
respective test-suite runs.

**Acceptance Scenarios**:

1. **Given** a clean checkout in the Workshop environment, **When** the
   contributor runs the Rust coverage command, **Then** the full workspace test
   suite runs and an HTML report plus a machine-readable export are produced
   under a known output path.
2. **Given** a clean checkout in the Workshop environment, **When** the
   contributor runs the Python coverage command, **Then** the offline test
   suite runs with branch coverage enabled and produces an HTML report, a
   terminal summary with missing lines, and a machine-readable export.
3. **Given** environment-gated integration tests (audio-server, desktop-session)
   whose gating variable is unset, **When** coverage runs, **Then** those tests
   skip cleanly exactly as they do today, and the report still generates
   (uncovered gated paths are visible, not fatal).

---

### User Story 2 - Use-case coverage and dead-code visibility (Priority: P2)

A maintainer exercises the program's real use-cases — a dictation session
against the fake-adapter server over the session socket in both wire dialects,
a WAV-file-driven session, a live-capture session, and the desktop publisher
path — under the same instrumentation. This works two ways: a scripted
exercise for repeatable runs, and — equally important — an **ad-hoc manual
mode**, documented in the project README, where the maintainer builds/runs any
component instrumented, pokes at it by hand as much as they like, and only
then asks for the report. Either way they get one merged report per language
that distinguishes three populations of code: covered by tests, covered only by
real use-case runs, and never executed at all. The never-executed population,
together with static unreferenced-code findings, is presented as an actionable
dead-code report.

**Why this priority**: This is the actual question the maintainer asked ("see
the dead code"), and it changes the meaning of the P1 numbers — code reachable
only from real use-cases is an integration-test gap, not dead code. It is P2
because it consumes the P1 instrumentation.

**Independent Test**: Run the scripted use-case exercise, merge its data with
the test-suite data, and verify the merged report attributes coverage and that
the dead-code report lists only code absent from both populations (plus static
findings), with no false positives for dynamically-loaded entry points.

**Acceptance Scenarios**:

1. **Given** test-suite coverage data from US1, **When** the maintainer runs
   the scripted use-case exercise under instrumentation and merges the results,
   **Then** the merged report shows which lines were hit by tests, which only
   by use-case runs, and which by neither.
2. **Given** the merged report, **When** the maintainer opens the dead-code
   summary, **Then** it lists never-executed regions per component, separated
   from statically-detected unreferenced items (unused functions, unused
   dependencies).
3. **Given** dynamically-dispatched entry points (model adapters loaded by
   name, test fixtures), **When** the static dead-code scan runs, **Then** a
   maintained allow-list suppresses known-dynamic symbols so the report stays
   actionable rather than noisy.
4. **Given** a use-case run that requires unavailable hardware (GPU adapters),
   **When** the exercise script reaches it, **Then** it skips with a clear
   notice and the report records the population as unexercised rather than
   failing.

---

### User Story 3 - CI enforces coverage on new changes (Priority: P2)

On every pull request, CI measures coverage for both languages in the same
Workshop environment used for the existing lint/test jobs, publishes the
reports as inspectable artifacts, and enforces a coverage threshold on the
lines the PR adds or changes (patch coverage), so a change that introduces
untested new code fails review mechanically. Whole-project coverage is reported
informationally and ratcheted later.

**Why this priority**: This converts coverage from a curiosity into a durable
quality gate and directly supports the constitution's review gates (red-green
evidence made visible). Equal rank with US2: US2 answers "what is dead", US3
answers "nothing new may be dead".

**Independent Test**: Open a PR that adds a covered function and a PR that adds
an uncovered function to the same component; the first passes the patch gate,
the second fails it with a report naming the uncovered new lines.

**Acceptance Scenarios**:

1. **Given** a pull request, **When** CI runs, **Then** coverage for both
   languages is measured in the Workshop environment and the reports are
   available as build artifacts.
2. **Given** a pull request whose changed lines meet the patch-coverage
   threshold, **When** the coverage gate evaluates, **Then** the check passes.
3. **Given** a pull request whose changed lines fall below the threshold,
   **When** the coverage gate evaluates, **Then** the check fails and names
   the uncovered changed lines.
4. **Given** whole-project coverage below any future project threshold,
   **When** the gate evaluates, **Then** the project level is reported
   informationally only and does not block merge (until explicitly ratcheted).
5. **Given** the patch-coverage mechanism, **When** it is implemented, **Then**
   it is self-hosted: machine-readable coverage exports from both toolchains
   are diffed against the PR's base branch by in-repo tooling, with the
   threshold enforced as a plain CI step — no coverage data leaves CI and no
   external service or account is required (decided 2026-07-26; the offline/
   privacy ethos outweighs hosted dashboards and PR annotations).

---

### User Story 4 - Expanded static checks in CI (Priority: P3)

A contributor's PR is automatically checked beyond today's clippy and pytest:
Rust formatting, unused/missing dependencies, Python lint and import hygiene,
scoped type-checking of the shared contract package, dead-code scans for both
languages, shell-script lint for dev scripts and snap hooks, workflow lint for
CI definitions, and dependency-advisory audits on a schedule rather than per PR.
A dependency-ban rule mechanically codifies the privacy invariant that the
shipped client must not gain a network stack.

**Why this priority**: High-value, low-risk hygiene that catches classes of
defects coverage cannot (unused deps, formatting drift, vulnerable advisories).
P3 because each check is independently adoptable and none blocks the coverage
stories.

**Independent Test**: For each adopted check, introduce a deliberate violation
on a branch and confirm the corresponding CI job fails with a legible message;
with violations removed, the job passes.

**Acceptance Scenarios**:

1. **Given** a Rust source file with formatting drift, **When** CI runs,
   **Then** the format check fails and names the file.
2. **Given** a workspace dependency no longer referenced by any crate, **When**
   the unused-dependency check runs, **Then** it fails and names the dependency.
3. **Given** Python code with unused imports or names, **When** the Python lint
   runs, **Then** CI fails and names them.
4. **Given** a change that adds a network-client dependency to the shipped
   dictation client, **When** the dependency-ban check runs, **Then** CI fails,
   enforcing the offline invariant mechanically.
5. **Given** a newly published advisory against a pinned dependency, **When**
   the scheduled audit runs, **Then** it opens visible signal (failing
   scheduled run or issue) without blocking unrelated PRs.

---

### User Story 5 - Spread evaluation for confined multi-system end-to-end tests (Priority: P3)

A maintainer evaluates the spread integration-test framework (as used by snapd)
against myna's confined-snap end-to-end needs and either adopts it — with an
initial suite that installs the client snap and a backend snap on a clean
virtual system, connects the `ubustt-socket` content share, and drives a WAV-file
dictation with assertions — or documents a reasoned rejection with the chosen
alternative. The evaluation explicitly covers: clean-system lifecycle
(prepare/execute/restore), multi-system matrices (supported Ubuntu releases),
CI feasibility on hosted runners with virtualization, audio without a physical
microphone (virtual audio source, per constitution Principle II), and debug
ergonomics.

**Why this priority**: The confined end-to-end seam is the repo's least-tested
shipped path (currently a single bespoke smoke job). Spread is the
ecosystem-standard tool for exactly this, but adoption is a real commitment, so
it is gated on a time-boxed evaluation. P3 and last: it is independent of the
coverage stories.

**Independent Test**: The evaluation concludes with a recorded decision; if
adopted, one spread task runs a confined dictation end-to-end on a clean
24.04 VM both locally and in CI, with audio provided by a virtual source, and
a failing assertion demonstrably fails the task.

**Acceptance Scenarios**:

1. **Given** the evaluation criteria (lifecycle, matrix, CI feasibility,
   virtual audio, debug ergonomics), **When** the spike completes, **Then** a
   written decision record states adopt-or-reject with reasons.
2. **Given** adoption, **When** the initial suite runs on a clean VM,
   **Then** it installs the confined snaps, connects the content share, drives
   a scripted dictation from recorded audio, and asserts the transcript —
   with no physical microphone.
3. **Given** adoption, **When** a task fails in CI, **Then** the maintainer
   can re-run it locally with an interactive debug session on the same VM.
4. **Given** the desktop-session use-cases (hotkey, injection, indicator),
   **When** the evaluation scopes them, **Then** they are explicitly deferred
   or included with a stated cost, not silently dropped.

### Edge Cases

- Gated integration tests (audio server, IBus/portal/GTK4 session) skip when
  their environment is absent; coverage produced on a bare CI runner
  under-reports those paths. Reports must make the gated population visible
  rather than silently absent, and full-fidelity coverage comes from the
  Workshop environment where the gates can be opened.
- Real-model (GPU) adapter tests are unavailable in CI and on most contributor
  machines; the populations they cover are reported as unexercised, never as
  failures.
- Coverage of subprocesses (the server process, spawned clients) is easy to
  lose silently; the merge step must fail loudly when expected data files are
  missing rather than producing a quietly-incomplete report.
- Patch-coverage thresholds interact with generated/vendored files and pure
  deletions; the gate must handle both without false failures (e.g., a
  deletion-only PR cannot be "under-covered").
- Snapshot/noise-sensitive thresholds: percentage gates must have sane
  rounding and floors so a one-line change in a small diff cannot flip the
  gate spuriously.
- Spread VMs have no microphone and limited desktop stack; tasks must use
  virtual audio sources and the fake adapter, and nested-virtualization
  availability on CI runners must be verified before the suite is relied on.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The project MUST provide a single named command, runnable in the
  canonical Workshop environment and in CI, that runs the full Rust workspace
  test suite with line and branch coverage and emits both a human-browsable
  report and a machine-readable export.
- **FR-002**: The project MUST provide an equivalent single named command for
  the Python test suite with branch coverage, a terminal missing-lines summary,
  a browsable report, and a machine-readable export.
- **FR-003**: Coverage commands MUST honour existing environment-gated test
  selection: gated suites skip cleanly when their gate is closed and contribute
  coverage when open, and report generation MUST succeed in both cases.
- **FR-004**: The project MUST provide a scripted use-case exercise — fake-
  adapter dictation sessions in both wire dialects, a recorded-audio (WAV)
  session, and the desktop publisher path — that runs the shipped programs
  under the same coverage instrumentation as the tests. A live-capture
  scenario MUST also exist but is environment-gated like the gated test
  suites: it runs where capture hardware is available and skips with a clear
  notice otherwise (consistent with the hardware-absence edge case).
- **FR-005**: The project MUST merge test-suite and use-case coverage into one
  report per language that distinguishes test-covered, use-case-only-covered,
  and never-executed code, and the merge MUST fail loudly when an expected
  data source is absent.
- **FR-006**: The project MUST produce a dead-code report combining
  never-executed regions (from FR-005) with static findings (unreferenced
  functions, unused dependencies, unused imports) for both languages, with a
  maintained allow-list for dynamically-dispatched entry points.
- **FR-007**: CI MUST measure Rust and Python coverage on every pull request
  in the same Workshop environment definition used by the existing lint/test
  jobs, and MUST publish the reports as build artifacts.
- **FR-008**: CI MUST enforce a configurable patch-coverage threshold on lines
  added or changed by a pull request, failing the check below threshold and
  naming the uncovered changed lines; deletion-only changes and
  generated/vendored paths MUST NOT cause false failures.
- **FR-009**: Whole-project coverage MUST be reported informationally in CI;
  it MUST NOT block merges until a project threshold is explicitly ratified in
  a later change.
- **FR-010**: CI MUST add static checks beyond the current set, at minimum:
  Rust format checking, Rust unused-dependency detection, Python lint
  (including unused imports/names), scoped type-checking of the shared
  contract package, shell-script lint for developer scripts and snap hooks,
  and workflow lint for CI definitions.
- **FR-011**: A dependency-policy check MUST mechanically prevent the shipped
  dictation client from acquiring network-client dependencies, codifying the
  offline invariant (constitution Principle V).
- **FR-012**: Dependency-advisory audits for both dependency locks MUST run on
  a schedule (not per PR) and surface visibly without blocking unrelated work.
- **FR-013**: All new local developer commands introduced by this feature MUST
  be expressed as named actions in the Workshop environment definition so CI
  and local runs share one command source (constitution Principle IV).
- **FR-014**: The spread evaluation MUST assess, at minimum, the five recorded
  criteria (clean-system lifecycle, multi-system matrix, hosted-runner CI
  feasibility, virtual-audio support per constitution Principle II, and debug
  ergonomics) and MUST conclude with a written adopt-or-reject decision record.
- **FR-015**: If spread is adopted, the initial suite MUST run a confined
  end-to-end dictation — client snap plus backend snap connected over the
  `ubustt-socket` content share, driven by recorded audio via a virtual
  source, with
  transcript assertion — on a clean VM of the current Ubuntu LTS, both locally
  and in CI, without a physical microphone.
- **FR-016**: This feature MUST NOT change the privacy posture of any shipped
  component: coverage and exercise runs MUST NOT persist audio or transcription
  content beyond the existing ephemeral test fixtures, and reports MUST NOT
  embed transcription content (constitution Principle V).
- **FR-017**: The project README MUST document, as a first-class workflow,
  how to build or launch every instrumentable component (all Rust client
  binaries, the Python server) in coverage-instrumented mode for ad-hoc
  manual testing, how to accumulate multiple manual runs without losing data,
  and how to produce the merged report and dead-code summary when finished —
  so a maintainer can answer "did my manual testing exercise this code?"
  without depending on the scripted exercise.

### Key Entities *(include if feature involves data)*

- **Coverage data set**: per-run, per-language line/branch hit records with
  provenance (which test or use-case run produced them); merged across runs;
  exported in browsable and machine-readable forms.
- **Coverage population**: a classification of code regions as test-covered,
  use-case-only-covered, or never-executed; the input to dead-code reporting
  and integration-gap analysis.
- **Dead-code report**: union of never-executed regions and static
  unreferenced-item findings, minus allow-listed dynamic entry points.
- **Patch-coverage verdict**: per pull request, the set of changed lines, their
  coverage state, and the pass/fail decision against the configured threshold.
- **Quality gate**: a named CI check (existing or new) with an explicit
  pass/fail contract; this feature adds coverage and static-check gates.
- **Spread decision record**: the written outcome of the framework evaluation,
  including the five criteria assessments and the adopt/reject conclusion.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A contributor can produce complete Rust and Python coverage
  reports from a clean environment with one command per language, each
  completing within the existing CI job budget plus no more than 2× overhead.
- **SC-002**: 100% of merged coverage reports correctly classify every code
  region into test-covered, use-case-only, or never-executed, verifiable by
  spot-checking known regions (e.g., a fake-adapter-only path shows
  test-covered; a desktop-session-gated path shows its true population).
- **SC-003**: The dead-code report names every never-referenced and
  never-executed region in both codebases with zero false positives for the
  allow-listed dynamic entry points.
- **SC-004**: 100% of pull requests receive a patch-coverage verdict; a
  demonstration PR adding uncovered code fails the gate and one adding covered
  code passes, with no false failures on deletion-only changes.
- **SC-005**: Every new static check is demonstrated to catch its target
  defect class once (deliberate-violation test) and passes clean on the main
  branch thereafter.
- **SC-006**: The spread evaluation concludes with a written decision within
  its time box; if adopted, the initial confined end-to-end suite passes on a
  clean VM locally and in CI with no physical audio hardware.
- **SC-007**: After this feature lands, no new code may be merged that the
  patch gate would flag as untested — the gate is mechanical, not advisory.
- **SC-008**: A maintainer can go from "I want to poke at the app by hand" to
  a merged coverage/dead-code report of their manual session using only the
  README instructions, in under 15 minutes, with no prior knowledge of the
  instrumentation tooling.

## Assumptions

- The canonical environment for coverage and new developer commands is the
  existing Workshop definition; CI continues to consume it, extending the
  current lint/test pattern rather than adding a parallel environment.
- Python coverage configuration already present in the repo (branch coverage,
  source scoping, the coverage plugin in the dev dependency group) is the
  starting point; this feature completes and operationalizes it rather than
  replacing it.
- The patch-coverage threshold default is 80% of changed lines, configurable;
  project-level thresholds are deferred until a baseline is known.
- The patch gate is self-hosted (no hosted coverage service); exports use
  widely-supported machine-readable formats so adopting a hosted service later
  would be a pure CI change, not a re-instrumentation.
- Source-based coverage instrumentation is preferred for Rust over
  binary-instrumentation alternatives, for accuracy and workspace support.
- The use-case exercise targets the fake adapter and recorded audio by
  default; GPU/real-model use-cases are optional enrichments that skip when
  hardware is absent, consistent with the existing test-suite behaviour.
- The desktop-session use-cases (literal hotkey, spoken injection) remain
  human acceptance activities; only the headless/publisher paths are
  automated under instrumentation.
- Spread evaluation time box is one focused effort (days, not weeks); rejection
  with a documented alternative is a successful outcome.
- Spread, if adopted, supplements and eventually supersedes the existing
  bespoke snap smoke job; unit, contract, and Workshop-level tests stay where
  they are.
- No code changes to shipped components are expected beyond testability seams
  already present; any that prove necessary follow red-green TDD per the
  constitution. CI configuration, Workshop actions, and evaluation-harness
  scripts are not subject to the TDD gate.
