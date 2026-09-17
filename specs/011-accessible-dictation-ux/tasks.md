# Tasks: Accessible Dictation UX

**Input**: Design documents from `/specs/011-accessible-dictation-ux/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Two tiers (plan Constitution Check), matching the precedent set in
feature 004's tasks.md.
- `myna-desktop` and `myna-cli` are shipped Rust system components: constitution
  Principle I applies in full — every behavior-bearing task is preceded by a
  failing hermetic test (fake `AccessibilityAnnouncer`, fake bus, fixed clock),
  and real-bus/real-hardware behavior is proven by env-gated suites
  (`MYNA_ATSPI_TESTS=1`, `MYNA_PIPEWIRE_TESTS=1`) runnable identically on the
  Workshop VM and hardware (Principle II).
- `extensions/myna-shell` is evaluation-harness-tier (plan Complexity Tracking):
  its pure modules (`a11y.js`'s `formatAnnouncement`, coverage-matrix loading)
  get GJS contract tests; the live `org.a11y.Bus` call and compositor behavior
  are proven by the manual acceptance protocol (quickstart.md), per research.md
  R6's headless-testing ceiling.

**Organization**: Tasks are grouped by user story. Priority order per spec.md:
US1, US2, US3 are co-P1 (ordered as they appear in spec.md); US4 is P2; US5 is
P3. Setup/Foundational/Polish carry no story label.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1/US2/US3/US4/US5 for story-phase tasks only
- All paths are repo-relative

## Path Conventions

- Rust, shared seam: `client/myna-desktop/src/accessibility/{mod,atspi,fake}.rs`,
  `client/myna-desktop/src/{sound/mod.rs,failure.rs,coverage.rs,preferences.rs}`
- Rust, extended existing files: `client/myna-desktop/src/{controller.rs,indicator/{mod,dbus,gtk,notify}.rs,shortcut/{mod.rs,portal.rs}}`
- Rust hermetic tests: co-located `#[cfg(test)]` modules + `client/myna-desktop/tests/{accessibility.rs,coverage.rs,failure.rs}`
- Rust env-gated tests: `client/myna-desktop/tests/{atspi_hw.rs,sound_hw.rs}` (gates: `MYNA_ATSPI_TESTS=1`, `MYNA_PIPEWIRE_TESTS=1`)
- CLI: `client/myna-cli/src/main.rs`, `client/myna-cli/tests/output.rs`
- GJS bundle: `extensions/myna-shell/{a11y.js,hud.js,states.js,coverage-matrix.json,test/{a11y,coverage}.test.js}`
- Shared data file: `extensions/myna-shell/coverage-matrix.json` (read by both languages — data-model.md)
- Shared contracts: `specs/011-accessible-dictation-ux/contracts/{announcer,coverage-matrix,failure-mapping,terminal-output}.md`

---

## Phase 1: Setup (Shared Infrastructure)

- [X] T001 Add the `atspi` crate dependency (workspace + `client/myna-desktop/Cargo.toml`), pinned to a version consistent with the vendored `zbus` major version (research.md R1)
- [X] T002 Bump the `ui-gtk` feature's `gtk4` version/feature flags in `client/myna-desktop/Cargo.toml` to expose `gtk_accessible_announce()` (stable since GTK 4.14)
- [X] T003 Extend `.workshop/myna.yaml` (constitution Principle IV) with an AT-SPI accessibility bus for the `MYNA_ATSPI_TESTS` suite (e.g. `at-spi2-core`'s bus launcher / `dbus-run-session`), alongside the existing session-D-Bus dependency
- [X] T004 [P] Create the shared coverage-matrix data file `extensions/myna-shell/coverage-matrix.json` per data-model.md's schema (all seven states, channels, `colour_only`/`sound_only: false`)
- [X] T005 [P] Scaffold empty modules with `mod` declarations (no logic yet): `client/myna-desktop/src/accessibility/{mod.rs,atspi.rs,fake.rs}`, `client/myna-desktop/src/{sound/mod.rs,failure.rs,coverage.rs,preferences.rs}`, wired into `client/myna-desktop/src/lib.rs`
- [X] T006 [P] Scaffold `extensions/myna-shell/a11y.js` (empty exports: `formatAnnouncement`, `Announcer`) and register it in `extensions/myna-shell/test/a11y.test.js` / `coverage.test.js` as empty placeholder suites

**Checkpoint**: workspace builds; new empty modules compile; coverage-matrix.json exists and is valid JSON.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the `AccessibilityAnnouncer` seam, the coverage-matrix invariant
gate, the `Preferences` read seam (verbosity/sound/silence-auto-stop, with a
hardcoded-default implementation per FR-004 pending the separate settings
feature — spec Assumptions), and the `FailurePresentation` registry
scaffold — all shared by every user story below.

**⚠️ CRITICAL**: no user-story work begins until this phase is complete.

### Preferences seam

- [X] T007 [P] Hermetic test in `client/myna-desktop/src/preferences.rs`: a `Preferences` trait (`verbosity()`, `sound_cues_enabled()`, `silence_auto_stop()`) has a `DefaultPreferences` impl returning `AllTransitions`, `true`, and the existing T59 default, matching FR-004/spec Assumptions exactly. **Write first, observe fail (trait doesn't exist), then implement**
- [X] T008 Implement `Preferences` trait + `DefaultPreferences` in `client/myna-desktop/src/preferences.rs`. Satisfies T007. Document in a doc-comment that a GSettings-backed implementation is the integration point for the separate settings feature (project-plan T54) and is out of scope here.

### `AccessibilityAnnouncer` seam (contracts/announcer.md A1–A5)

- [X] T009 [P] Hermetic test in `client/myna-desktop/src/accessibility/fake.rs`: a `FakeAnnouncer` records every `announce()` call's text/severity, and `set_state()` calls separately, for assertion by other tests. **Write first, observe fail, then implement**
- [X] T010 Define the `AccessibilityAnnouncer` trait in `client/myna-desktop/src/accessibility/mod.rs` (contracts/announcer.md) + implement `FakeAnnouncer`. Satisfies T009.
- [X] T011 [P] Hermetic test: `announce()` is never called with the raw transcript or unstable-hypothesis text — a fixture that would fail if any caller ever passed transcript content through (A1, constitution V). Add to `client/myna-desktop/tests/accessibility.rs`. **Write first, observe fail (no gating exists yet), then implement**
- [X] T012 Implement a `content_free` debug-assertion/type-level guard (e.g. a distinct `AnnouncementText` newtype constructed only from static/known-safe strings, never `String::from(transcript)`) in `accessibility/mod.rs`. Satisfies T011. *(Implemented as a compile-time guard: `AnnouncementText` wraps `&'static str` with no `String`/non-`'static` constructor — a transcript, always an owned `String`, cannot be passed to `announce()`/`set_state()` at all. Regression-tested by `accessibility::tests::announcement_text_round_trips_a_static_literal`.)*
- [X] T013 [P] Hermetic test in `client/myna-desktop/tests/accessibility.rs`: with `Preferences::verbosity() == Off`, `announce()` calls are suppressed but `set_state()` still updates queryable name/description (A2, FR-004). **Write first, observe fail, then implement** *(implemented as `VerbosityGatedAnnouncer` tests in `accessibility/gate.rs`, co-located per repo convention)*
- [X] T014 [P] Hermetic test: with `verbosity() == FailuresOnly`, only `Critical`/`Recoverable`-severity announcements pass through (A3). **Write first, observe fail, then implement**
- [X] T015 Implement the verbosity gate (a small `VerbosityGatedAnnouncer` wrapper or gating logic in `controller.rs`'s transition-to-announcement path) satisfying T013/T014.
- [X] T016 [P] Hermetic test with a fake clock: a burst of transitions within the coalescing window produces at most one delivered announcement; a superseded announcement is never delivered after its state has passed (A4, FR-005, SC-003). **Write first, observe fail, then implement** *(used `tokio::time` paused/advance rather than a hand-rolled clock trait — equivalent fake-clock determinism, no new abstraction needed)*
- [X] T017 Implement coalescing/supersession in the same gating layer as T015. Satisfies T016. *(Implemented as `CoalescingAnnouncer`: a single debounce-actor task per instance, not per-call spawns, so "latest wins" is structural.)*
- [X] T018 [P] Hermetic test: if the underlying `announce()` call errors, the caller's session flow (asserted via a fake controller/session harness) continues unaffected, and the failure becomes a `Recoverable` `FailurePresentation` rather than being swallowed (A5, FR-002a). **Write first, observe fail, then implement** *(harness = `FakeAnnouncer` + `RecoveringAnnouncer` wrapper, not full `controller.rs` — sufficient to prove the property in isolation; controller.rs wiring happens in US1's T036)*
- [X] T019 Implement `RecoveringAnnouncer` in `client/myna-desktop/src/accessibility/recover.rs`: swallows `announce()` errors (always returns `Ok(())` to the caller) and records a `Recoverable` `FailurePresentation`, retrievable via `take_recovery_notice()`. Satisfies T018. *(Actual `controller.rs` wiring deferred to US1 T036, where the announcer is first wired into real transitions.)*

### Coverage-matrix gate (contracts/coverage-matrix.md C1–C5, K1–K2)

- [X] T020 [P] Hermetic test in `client/myna-desktop/tests/coverage.rs`: loading `extensions/myna-shell/coverage-matrix.json` and asserting every entry has non-empty `visual`/`non_visual` arrays and `colour_only`/`sound_only == false` (C1–C3). **Write first, observe fail (loader doesn't exist), then implement** *(co-located in `client/myna-desktop/src/coverage.rs` per repo convention)*
- [X] T021 Implement the matrix loader + invariant checks in `client/myna-desktop/src/coverage.rs`. Satisfies T020.
- [X] T022 [P] Hermetic test: every `DictationState`/`IndicatorState` variant has a matching matrix entry by `id` (C4) — a new state added without a matrix entry fails this test. **Write first, observe fail, then implement**
- [X] T023 Implement the exhaustiveness check (e.g. a `match` over the enum returning each arm's expected `id`, compared against the loaded matrix). Satisfies T022. *(Implemented against the existing `indicator::dbus::wire_state` constants — the crate's actual wire-state source of truth — rather than a new match.)*
- [X] T024 [P] GJS contract test in `extensions/myna-shell/test/coverage.test.js`: same C1–C3 invariants plus C5 (every `states.js` descriptor id has a matching matrix entry), reading the same `coverage-matrix.json`. **Write first, observe fail, then implement**
- [X] T025 Implement the GJS-side loader/check in a small pure function within `extensions/myna-shell/a11y.js` (or a sibling module). Satisfies T024. *(Implemented as a sibling `extensions/myna-shell/coverage.js` module to keep `a11y.js` focused on announcements.)*
- [X] T026 [P] Hermetic Rust test: a WCAG contrast-ratio calculator computes ≥4.5:1 for every declared text colour pair and ≥3:1 for non-text pairs parsed from `extensions/myna-shell/stylesheet.css` (K1/K2, FR-013). **Write first, observe fail, then implement** *(colour pairs hand-extracted as constants rather than CSS-parsed — see `coverage::stylesheet_colours` doc comment for rationale; worst-case compositing over both white and black backgrounds handles the pill's translucency)*
- [X] T027 Implement the contrast calculator + CSS colour-pair extraction in `client/myna-desktop/src/coverage.rs` (or a sibling `contrast.rs`). Satisfies T026.

### `FailurePresentation` registry scaffold (contracts/failure-mapping.md F1, F3 — full mapping deferred to US4)

- [X] T028 [P] Hermetic test in `client/myna-desktop/src/failure.rs`: a `FailurePresentation { id, message, recovery_action, severity }` struct exists and a registry keyed by stable `id` returns the same instance for repeated lookups of the same `id` (F3 — no per-call-site divergence). **Write first, observe fail, then implement**
- [X] T029 Implement the `FailurePresentation` struct + keyed registry (empty of real entries for now — populated in US4) in `client/myna-desktop/src/failure.rs`. Satisfies T028.

**Checkpoint**: the announcer seam, verbosity/coalescing gating, coverage-matrix gate, and failure-presentation scaffold are all in place and tested. User-story work can now begin.

---

## Phase 3: User Story 1 — A blind user dictates without looking at the screen (Priority: P1) 🎯 MVP

**Goal**: Every state transition is proactively announced via AT-SPI from both
`myna-desktop`'s shipped indicators and the Shell HUD, without moving focus,
and the current state is programmatically queryable at any time.

**Independent Test**: With a screen reader running and the display off,
complete a full dictation and then trigger an error; every transition and the
failure are announced within budget (quickstart.md Scenarios 1–2).

### Tests for User Story 1 (write first, observe fail)

- [ ] T030 [P] [US1] Hermetic test in `client/myna-desktop/tests/accessibility.rs`: `controller.rs`'s transition handling calls `AccessibilityAnnouncer::announce()` exactly once per `loading→listening→transcribing→finishing→idle` transition the current verbosity includes, using the `FakeAnnouncer` (Acceptance Scenario 2)
- [ ] T031 [P] [US1] Hermetic test: `announce()`/`set_state()` are never accompanied by any focus-changing call in the fake indicator/controller harness (Acceptance Scenario 1, FR-002, FR-022)
- [ ] T032 [P] [US1] Env-gated integration test `client/myna-desktop/tests/atspi_hw.rs` (`MYNA_ATSPI_TESTS=1`): the real `atspi`-backed announcer connects to `org.a11y.Bus`, emits an `Announcement` event, and its accessible object's name/description are queryable via the `atspi` crate's client-side proxy at an arbitrary moment, not only at a transition (A6, FR-001)
- [ ] T033 [P] [US1] Hermetic watermark test: constructing/registering the `atspi`-backed announcer and never calling `announce()` costs no measurable overhead versus not constructing it at all (A7, FR-007) — record the baseline per constitution Principle III
- [ ] T034 [P] [US1] Hermetic test: `GtkIndicator`'s formatted announcement text (behind `ui-gtk`) matches the text the `atspi`-backed announcer would send for the same state, via a shared fixture (A8, FR-006)

### Implementation for User Story 1

- [ ] T035 [US1] Implement the real `atspi`-backed `AccessibilityAnnouncer` in `client/myna-desktop/src/accessibility/atspi.rs`: connect to `org.a11y.Bus`, register one accessible object for the dictation session, emit `Announcement` on `announce()`, update name/description on `set_state()`. Satisfies T032/T033.
- [ ] T036 [US1] Wire `controller.rs` to call the announcer (via the Foundational gating layer) on every `DictationState` transition, alongside the existing `Indicator::set_state()` call. Satisfies T030.
- [ ] T037 [US1] Audit `controller.rs`/`indicator/*.rs` announcement call sites to confirm none touch keyboard focus (no `gtk::Widget::grab_focus`-equivalent, no window activation). Satisfies T031.
- [ ] T038 [US1] [P] Extend `client/myna-desktop/src/indicator/gtk.rs` (behind `ui-gtk`) to call `gtk_accessible_announce()` with the same formatted text as T035, sourced from one shared formatting function. Satisfies T034.
- [ ] T039 [US1] Implement `formatAnnouncement(stateId, severity)` as a pure function in `extensions/myna-shell/a11y.js`, matching contract G1 (cross-checked against the Rust formatting fixture from T034/T038).
- [ ] T040 [US1] [P] GJS contract test in `extensions/myna-shell/test/a11y.test.js`: `formatAnnouncement` returns the expected text/politeness for every coverage-matrix state (G1). **Write first, observe fail, then satisfied by T039**
- [ ] T041 [US1] Implement the `Announcer` class in `extensions/myna-shell/a11y.js`: opens `org.a11y.Bus` via `Gio.DBusConnection`, emits the `Announcement` event, coalesces bursts using the same window constant as the Rust side (G2, contract announcer.md).
- [ ] T042 [US1] [P] GJS contract test: `Announcer`'s coalescing behavior matches A4's rule using a fake/injected clock (G2). **Write first, observe fail, then satisfied by T041**
- [ ] T043 [US1] Wire `extensions/myna-shell/hud.js` to call the `Announcer` on every state/severity transition, and release the bus connection in `disable()` (mirrors feature 004's `dbus.js` lifecycle contract, G3).
- [ ] T044 [US1] [P] Braille-path documentation check: confirm (and record in `quickstart.md` if not already) that the AT-SPI `Announcement` event reaches a connected braille display through the same path as speech — no code change expected, but the manual acceptance scenario must explicitly include it (Acceptance Scenario 3).
- [ ] T045 [US1] Update `docs/project-plan.md` T56 entry to record this feature as closing the gap it tracked.

**Checkpoint**: US1 is independently functional — announcements flow from both `myna-desktop` and the Shell HUD, focus is never touched, and state is queryable on demand.

---

## Phase 4: User Story 2 — No-chord, no-hold activation (Priority: P1)

**Goal**: Activation remains tap-to-toggle by default, works under sticky
keys/slow keys/autorepeat without duplicate sessions, and every
acknowledgement path (including error dismissal) works without a pointer.

**Independent Test**: With only a single non-repeating activation event
available, start, dictate, and end a session two ways (second tap; silence);
both succeed with no held/chorded key (quickstart.md Scenarios 3–4).

### Tests for User Story 2 (write first, observe fail)

- [ ] T046 [P] [US2] Hermetic test in `client/myna-desktop/src/shortcut/portal.rs` (or its existing test module): `ActivationMode::Toggle` remains `#[default]` — a regression test asserting the default explicitly, so any future change is a deliberate, test-visible decision (FR-015 non-regression).
- [ ] T047 [P] [US2] Hermetic test: a simulated autorepeat burst of the same key-down event within a short window produces exactly one toggle edge, not one per repeat (FR-017). **Write first, observe fail, then implement**
- [ ] T048 [P] [US2] Hermetic test: every user-facing acknowledgement action currently exposed only via a pointer/hover path (e.g. the notification dismiss (×) if present) has a non-pointer equivalent (keyboard activation or auto-timeout) asserted against the indicator's action list (FR-019).

### Implementation for User Story 2

- [ ] T049 [US2] Implement autorepeat de-duplication in `client/myna-desktop/src/shortcut/mod.rs` (a debounce/edge-detection window). Satisfies T047.
- [ ] T050 [US2] Audit and, where needed, add a non-pointer path (e.g. an explicit key binding or timeout) for any indicator acknowledgement action found pointer-only in T048; wire through `indicator/{dbus,gtk,notify}.rs`.
- [ ] T051 [US2] [P] Add/confirm a manual verification note in `quickstart.md` Scenario 4 covering sticky-keys/slow-keys behavior specifically (cannot be simulated hermetically — GNOME accessibility features live in the compositor/input stack, not `myna-desktop`).
- [ ] T052 [US2] [P] Confirm (test if feasible, else document as manual) that activating dictation via the desktop's own switch-access/dwell-click produces an ordinary key/pointer event indistinguishable from a normal shortcut activation — no Myna-side accommodation needed (FR-020). Record the finding in `quickstart.md`.

**Checkpoint**: US2 is independently verifiable — no regression in tap-to-toggle, autorepeat is de-duplicated, and every acknowledgement has a non-pointer path.

---

## Phase 5: User Story 3 — Every state is perceivable in more than one way (Priority: P1)

**Goal**: Every state/severity has both a visual and non-visual channel, no
colour-only/sound-only encoding, reduced-motion/high-contrast/text-scale are
honoured, and sound cues (once added) don't degrade WER.

**Independent Test**: The coverage matrix passes its completeness gate; a
session run at 200% text scale / high contrast / forced colours / reduced
motion remains complete and legible (quickstart.md Scenario 5).

### Tests for User Story 3 (write first, observe fail)

- [ ] T053 [P] [US3] Hermetic test: the coverage-matrix gate (T020–T027, already passing) is re-run as part of this story's CI-facing suite to confirm SC-002 end-to-end (no new logic — a regression-gate placement task, not new production code).
- [ ] T054 [P] [US3] Hermetic test in `client/myna-desktop/src/sound/mod.rs`: a `SoundCue` player exposes `play(cue: CueKind)` behind a `Preferences::sound_cues_enabled()`/per-cue-toggle gate, using a fake PipeWire sink for the hermetic tier. **Write first, observe fail, then implement**
- [ ] T055 [P] [US3] Env-gated integration test `client/myna-desktop/tests/sound_hw.rs` (`MYNA_PIPEWIRE_TESTS=1`): a real PipeWire playback stream plays a short embedded cue clip without error.
- [ ] T056 [P] [US3] Hermetic test: cue playback never blocks or delays the capture path — asserted by timing the controller's transition handling with sound cues enabled vs. disabled and asserting no measurable difference (FR-011 non-blocking half; the WER half is measured separately, T060).

### Implementation for User Story 3

- [ ] T057 [US3] Implement `SoundCue`/playback in `client/myna-desktop/src/sound/mod.rs`, reusing `myna-audio`'s vendored `pipewire` crate for an output stream (research.md R2). Satisfies T054/T055.
- [ ] T058 [US3] Wire `controller.rs` to trigger `SoundCue::play()` on session start/end/failure, decoupled from (never blocking) the capture/announcement path. Satisfies T056.
- [ ] T059 [US3] [P] Confirm/extend `extensions/myna-shell/accent.js`'s existing reduced-motion/high-contrast GSettings reads (feature 004) cover FR-012's full list (text-scale, forced-colours) — add any missing read, with a GJS contract test for the fallback behavior.
- [ ] T060 [US3] Run the real-corpus WER benchmark (`dev/fetch_real_corpus.py` family) with sound cues + announcements enabled vs. a silent baseline; record the delta as a checked-in watermark, asserting ≤0.5pp (SC-006, constitution Principle III). Document the run in `quickstart.md` Scenario 6.
- [ ] T061 [US3] [P] Manual verification pass (recorded in `quickstart.md` Scenario 5) at 200% text scale, high-contrast theme, forced colours, and reduced motion — confirm no clipping/truncation and the reduced-motion static equivalent for live capture.

**Checkpoint**: US3 is independently verifiable — coverage matrix, contrast, sound cues, and reduced-motion/high-contrast handling are all gated.

---

## Phase 6: User Story 4 — Failures explain themselves in plain language (Priority: P2)

**Goal**: Every known failure maps to one plain-language `FailurePresentation`
rendered identically across the indicator, notification, terminal, and
announcement; recoverable vs. critical is distinguishable without colour.

**Independent Test**: Force each failure in the taxonomy; every one produces a
single plain-language message consistent across all four surfaces
(quickstart.md Scenario 2).

### Tests for User Story 4 (write first, observe fail)

- [ ] T062 [P] [US4] Hermetic test in `client/myna-desktop/tests/failure.rs`: every known failure source today (`controller.rs`'s `OrchestratorEvent::Error` messages, `inject::InjectError`'s `SecureField`/`NoTarget`/`Unavailable`/`Backend` variants) has a registered `FailurePresentation` with a non-empty, jargon-free `message` (F1, FR-023). **Write first, observe fail, then implement**
- [ ] T063 [P] [US4] Hermetic test: a single shared fixture renders the same `FailurePresentation` through the indicator-error path, the `notify-rust` toast path, `myna-cli`'s stderr, and the announcer, and asserts identical `message`/`recovery_action` text across all four (F2, FR-024).
- [ ] T064 [P] [US4] Hermetic test: two different call sites referencing the same failure `id` never diverge in wording (F3, FR-024a).
- [ ] T065 [P] [US4] Hermetic test: `Critical` presentations remain queryable/persistent until acknowledged; `Recoverable` ones auto-dismiss but are still retrievable afterward via a small in-memory "last notice" accessor (F4, FR-025/026).
- [ ] T066 [P] [US4] Hermetic test with a fake clock: a long-running operation (model loading) past a defined threshold produces an actionable `FailurePresentation`-shaped message distinguishable from ordinary periodic progress (F5, FR-027).

### Implementation for User Story 4

- [ ] T067 [US4] Populate the `FailurePresentation` registry in `client/myna-desktop/src/failure.rs` with one entry per known failure source. Satisfies T062.
- [ ] T068 [US4] Route `controller.rs`'s error handling, `indicator/notify.rs`'s toast text, and the announcer's failure announcements all through `failure::lookup(id)` rather than ad-hoc strings. Satisfies T063/T064.
- [ ] T069 [US4] Implement the "last notice" accessor + critical-persistence/recoverable-auto-dismiss timing in `controller.rs`. Satisfies T065.
- [ ] T070 [US4] Implement the long-operation progress/threshold logic (periodic non-visual progress indication + actionable message past threshold) in `controller.rs`, reusing the announcer for the periodic pulse. Satisfies T066.
- [ ] T071 [US4] [P] Wire `client/myna-cli/src/main.rs` to render failures via `failure::lookup(id)` on stderr (shared with US5's terminal-output work — coordinate, don't duplicate).

**Checkpoint**: US4 is independently verifiable — every known failure has one plain-language presentation reused everywhere.

---

## Phase 7: User Story 5 — The terminal client is readable by a screen reader (Priority: P3)

**Goal**: `myna-cli` output remains fully meaningful with colour/emoji
stripped, is line-oriented with no redraw/animation, and failures use the same
plain language as every other surface.

**Independent Test**: Run under a screen reader with colour disabled; state,
results, and failures are all understandable from the plain-text stream alone
(quickstart.md Scenario 8).

### Tests for User Story 5 (write first, observe fail)

- [ ] T072 [P] [US5] Hermetic test in `client/myna-cli/tests/output.rs`: with colour disabled, every state transition and result line contains an explicit textual marker (e.g. `[listening]`) and no ANSI colour codes (T1, FR-028). **Write first, observe fail, then implement**
- [ ] T073 [P] [US5] Hermetic test: every stdout write ends in `\n` and contains no carriage-return/cursor-movement ANSI sequences across a simulated multi-transition session (T2, FR-029).
- [ ] T074 [P] [US5] Hermetic test: a forced failure is written to stderr using the exact `FailurePresentation` text from `failure::lookup` (T3, FR-029) — shares the fixture from T063.

### Implementation for User Story 5

- [ ] T075 [US5] Implement colour/emoji stripping + explicit textual state markers in `client/myna-cli/src/main.rs`. Satisfies T072.
- [ ] T076 [US5] Remove/replace any in-place redraw or spinner-animation output in `client/myna-cli/src/main.rs` with append-only line output. Satisfies T073.
- [ ] T077 [US5] Wire failure rendering to `failure::lookup` (shared with T071). Satisfies T074.

**Checkpoint**: All five user stories are independently functional.

---

## Phase 8: Polish & Cross-Cutting Concerns

- [ ] T078 [P] Run `quickstart.md` Scenarios 1–8 manually against a real GNOME session (dev build) and record results/gaps directly in `quickstart.md`.
- [ ] T079 [P] Repeat the subset of `quickstart.md` scenarios that can run against the packaged, strictly confined `myna` snap (FR-033/SC-009); record any confinement-specific finding (e.g. `desktop` plug sufficiency for `org.a11y.Bus`).
- [ ] T080 [P] Update `docs/desktop-injection.md` / `docs/project-plan.md` to reflect the new `accessibility`/`sound`/`failure`/`coverage` modules and close out T56; annotate T31/T54/T58/T59/T61/T62 with this feature's overlap resolution (per spec Assumptions).
- [ ] T081 [P] Add CI wiring (if not automatic via existing `cargo test`/`workshop run` targets) to ensure `MYNA_ATSPI_TESTS`/`MYNA_PIPEWIRE_TESTS`-gated suites and the GJS coverage/contrast tests run on every PR (constitution Development Workflow gate).
- [ ] T082 Full regression pass: `make test-client`, `make test-extension`, and the gated suites all green together.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies.
- **Foundational (Phase 2)**: depends on Setup — BLOCKS all user stories.
- **User Stories (Phases 3–7)**: all depend on Foundational. US1/US2/US3 (all
  P1) have no dependency on each other and may proceed in parallel. US4 reuses
  the `FailurePresentation` scaffold from Foundational and is independent of
  US1–US3's specific work (though it benefits from US1's announcer being live
  to route failure announcements through). US5 reuses US4's `failure::lookup`
  (T071/T077 coordinate on the same call) but is otherwise independent.
- **Polish (Phase 8)**: depends on all desired user stories being complete.

### Within Each User Story

- Tests MUST be written and FAIL before implementation (red-green TDD,
  constitution Principle I).
- Foundational seams (announcer, coverage gate, failure registry) before
  story-specific wiring.
- `myna-desktop` implementation before the mirrored GJS implementation, since
  the GJS side's contract tests (G1/G2) are cross-checked against Rust-side
  fixtures.

### Parallel Opportunities

- All `[P]`-marked tasks within a phase touch different files and can run in
  parallel.
- US1, US2, and US3 can be staffed and merged in parallel once Foundational is
  merged (see Branch Staging Plan).
- US4 and US5 can similarly proceed in parallel with each other, and with
  US1–US3, once Foundational is merged, though US5 should land after or
  alongside US4 to avoid two people editing `failure::lookup`'s call sites in
  `myna-cli` at once.

---

## Implementation Strategy

### Branch Staging Plan (constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|------------------------|-------------|
| 1 | `011-accessible-dictation-ux-foundation` | Phase 1–2 (Setup, Foundational) | — | hermetic suite (`cargo test`, GJS contract tests) |
| 2 | `011-accessible-dictation-ux-us1` | Phase 3 (US1) | #1 | hermetic + `MYNA_ATSPI_TESTS` integration |
| 3 | `011-accessible-dictation-ux-us2` | Phase 4 (US2) | #1 | hermetic suite |
| 4 | `011-accessible-dictation-ux-us3` | Phase 5 (US3) | #1 | hermetic + `MYNA_PIPEWIRE_TESTS` integration + WER watermark (T060) |
| 5 | `011-accessible-dictation-ux-us4` | Phase 6 (US4) | #1 (and ideally after #2 for live announcement routing, though not blocking) | hermetic suite |
| 6 | `011-accessible-dictation-ux-us5` | Phase 7 (US5) | #5 (shares `failure::lookup` wiring) | hermetic suite |
| 7 | `011-accessible-dictation-ux-polish` | Phase 8 | #2, #3, #4, #5, #6 | full regression (`make test-client`, `make test-extension`, gated suites) |

Each branch contains its own tests and implementation together (red-green
within the branch); a branch does not build on unmerged sibling work — US2/US3/
US4 all branch from the merged foundation branch (#1), not from each other or
from #2.

### MVP First (User Story 1 Only)

1. Phase 1: Setup
2. Phase 2: Foundational (blocks everything)
3. Phase 3: User Story 1
4. **STOP and VALIDATE**: run quickstart.md Scenarios 1–2 manually; confirm
   `MYNA_ATSPI_TESTS` suite passes.
5. This closes the largest documented gap (T56) and satisfies SC-001/SC-003 on
   its own.

### Incremental Delivery

1. Setup + Foundational → foundation ready, merged.
2. US1 → validate independently → merge (MVP).
3. US2, US3 → validate independently → merge (either order; both P1).
4. US4 → validate independently → merge.
5. US5 → validate independently → merge.
6. Polish → full regression → merge.
