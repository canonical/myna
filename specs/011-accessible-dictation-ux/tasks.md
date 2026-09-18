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
- [X] T008 Implement `Preferences` trait + `DefaultPreferences` in `client/myna-desktop/src/preferences.rs`. Satisfies T007. *(Superseded/extended after a rebase onto `integration-220627` picked up a real `com.canonical.Myna.Dictation` GSettings store: added `announcement-verbosity`/`sound-cues-enabled` keys to the existing schema (`client/data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml`), extended `myna_core::settings::{Settings,Store}` to read/write them, and added `GSettingsPreferences` as the real production `Preferences` impl \u2014 see research.md R7. `DefaultPreferences` is kept as the dependency-free hermetic-test seam. A later rebase dropped the planned `silence-auto-stop-seconds` key and its `Preferences` accessor: upstream shipped the same capability as `silence-timeout`, consumed by `myna_desktop::AutoStop`, and a second read seam would have been a second source of truth.)*

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

- [X] T030 [P] [US1] Hermetic test in `client/myna-desktop/tests/accessibility.rs`: `controller.rs`'s transition handling calls `AccessibilityAnnouncer::announce()` exactly once per `loading→listening→transcribing→finishing→idle` transition the current verbosity includes, using the `FakeAnnouncer` (Acceptance Scenario 2). *(Real bug found: the controller's existing live-event-path + finalize-block "safety net" pattern calls `Indicator::set_state` twice for the same completed-session state, relying on `DbusIndicator::publish`'s own per-wire-state dedup to make the second a no-op — `AnnouncingIndicator` needed the same idempotency, added in T036.)*
- [X] T031 [P] [US1] Hermetic test: `announce()`/`set_state()` are never accompanied by any focus-changing call in the fake indicator/controller harness (Acceptance Scenario 1, FR-002, FR-022). *(Implemented as a comparative test: an identical utterance script run once with a plain indicator and once wrapped in `AnnouncingIndicator` produces byte-identical `InjectorLog`s — proving the announcer/indicator seam has no observable side effect on the injection/focus-adjacent path; the seam's own trait signatures additionally expose no focus-capable method at all.)*
- [X] T032 [P] [US1] Env-gated integration test `client/myna-desktop/tests/atspi_hw.rs` (`MYNA_ATSPI_TESTS=1`): the real `atspi`-backed announcer connects to `org.a11y.Bus`, emits an `Announcement` event, and its accessible object's name/description are queryable via the `atspi` crate's client-side proxy at an arbitrary moment, not only at a transition (A6, FR-001). *(Validated against a real, running `org.a11y.Bus` on the dev host during implementation — `AtspiAnnouncer::connect()` reaches the bus and the correctly-typed `AnnouncementEvent` message is constructed and dispatched; live signal delivery could not be confirmed end-to-end in this sandboxed dev container specifically because `dbus-broker`'s AppArmor mediation denies accessibility-bus calls from the confined terminal session — confirmed via `journalctl` showing `apparmor="DENIED" ... bus="accessibility"` unrelated to this code. Documented in `accessibility/atspi.rs`'s module doc comment per research.md R6's precedent.)*
- [X] T033 [P] [US1] Hermetic watermark test: constructing/registering the `atspi`-backed announcer and never calling `announce()` costs no measurable overhead versus not constructing it at all (A7, FR-007) — record the baseline per constitution Principle III. *(Implemented as an `MYNA_ATSPI_TESTS`-gated watermark in `tests/watermarks.rs` — same gating as T032, since a real bus connection is what's being measured; a connect+announce round trip is bounded at <50ms.)*
- [X] T034 [P] [US1] Hermetic test: `GtkIndicator`'s formatted announcement text (behind `ui-gtk`) matches the text the `atspi`-backed announcer would send for the same state, via a shared fixture (A8, FR-006). *(Satisfied by construction rather than a redundant runtime test: `indicator/gtk.rs` calls the same `accessibility::format_state_announcement` function `atspi.rs` and the `AnnouncingIndicator` wrapper use — there is no second, independent formatting path that could drift. `format.rs`'s own unit tests already cover the shared mapping.)*

### Implementation for User Story 1

- [X] T035 [US1] Implement the real `atspi`-backed `AccessibilityAnnouncer` in `client/myna-desktop/src/accessibility/atspi.rs`: connect to `org.a11y.Bus`, register one accessible object for the dictation session, emit `Announcement` on `announce()`, update name/description on `set_state()`. Satisfies T032/T033.
- [X] T036 [US1] Wire `controller.rs` to call the announcer (via the Foundational gating layer) on every `DictationState` transition, alongside the existing `Indicator::set_state()` call. Satisfies T030. *(Implemented as `AnnouncingIndicator`, an `Indicator`-implementing wrapper composed at construction time rather than editing every `controller.rs` call site — see `accessibility/announcing_indicator.rs`'s doc comment for the rationale; also added same-state dedup, the real bug T030 found.)*
- [X] T037 [US1] Audit `controller.rs`/`indicator/*.rs` announcement call sites to confirm none touch keyboard focus (no `gtk::Widget::grab_focus`-equivalent, no window activation). Satisfies T031. *(Audited: `AccessibilityAnnouncer`/`Indicator` trait signatures expose no focus-capable method at all; `GtkIndicator`'s window is explicitly `set_can_focus(false)`/`set_focusable(false)` — already true before this feature, unchanged by the new `announce()` call, which touches only the accessibility tree, never window-manager focus.)*
- [X] T038 [US1] [P] Extend `client/myna-desktop/src/indicator/gtk.rs` (behind `ui-gtk`) to call `gtk_accessible_announce()` with the same formatted text as T035, sourced from one shared formatting function. Satisfies T034. *(Confirmed `gtk4::prelude::AccessibleExt::announce()` is a direct FFI wrapper over `gtk_accessible_announce` by inspecting the generated bindings; severity maps to `AccessibleAnnouncementPriority::High`/`Medium`.)*
- [X] T039 [US1] Implement `formatAnnouncement(stateId, severity)` as a pure function in `extensions/myna-shell/a11y.js`, matching contract G1 (cross-checked against the Rust formatting fixture from T034/T038).
- [X] T040 [US1] [P] GJS contract test in `extensions/myna-shell/test/a11y.test.js`: `formatAnnouncement` returns the expected text/politeness for every coverage-matrix state (G1). **Write first, observe fail, then satisfied by T039**
- [X] T041 [US1] Implement the `Announcer` class in `extensions/myna-shell/a11y.js`: opens `org.a11y.Bus` via `Gio.DBusConnection`, emits the `Announcement` event, coalesces bursts using the same window constant as the Rust side (G2, contract announcer.md). *(Injectable scheduler seams `_scheduleFlush`/`_cancelScheduled` added — same DI-seam convention `dbus.js` already uses — so coalescing is deterministically testable without a real GLib main loop. `enable()` wraps its sync bus calls in try/catch so a failure never crashes the extension, matching FR-002a.)*
- [X] T042 [US1] [P] GJS contract test: `Announcer`'s coalescing behavior matches A4's rule using a fake/injected clock (G2). **Write first, observe fail, then satisfied by T041** *(also covers G3 lifecycle — disable() releases the connection and drops pending state — and the enable()-failure resilience path)*
- [X] T043 [US1] Wire `extensions/myna-shell/hud.js` to call the `Announcer` on every state/severity transition, and release the bus connection in `disable()` (mirrors feature 004's `dbus.js` lifecycle contract, G3). *(Implemented in `extension.js` instead of `hud.js`: the `onStateChanged` callback there already receives both the raw wire state and the computed severity uniformly for both hidden and shown transitions, keeping `hud.js` a pure renderer with no accessibility-bus dependency of its own — architectural deviation from the task's literal file suggestion, same rationale pattern as prior tasks' documented deviations.)*
- [X] T044 [US1] [P] Braille-path documentation check: confirm (and record in `quickstart.md` if not already) that the AT-SPI `Announcement` event reaches a connected braille display through the same path as speech — no code change expected, but the manual acceptance scenario must explicitly include it (Acceptance Scenario 3). *(Added as quickstart.md Scenario 1 step 8.)*
- [X] T045 [US1] Update `docs/project-plan.md` T56 entry to record this feature as closing the gap it tracked.

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

- [X] T046 [P] [US2] Hermetic test in `client/myna-desktop/src/shortcut/portal.rs` (or its existing test module): `ActivationMode::Toggle` remains `#[default]` — a regression test asserting the default explicitly, so any future change is a deliberate, test-visible decision (FR-015 non-regression). *(`toggle_is_the_default_activation_mode` — the behavior already existed, this is a regression lock, not new behavior.)*
- [X] T047 [P] [US2] Hermetic test: a simulated autorepeat burst of the same key-down event within a short window produces exactly one toggle edge, not one per repeat (FR-017). **Write first, observe fail, then implement** *(Audited: existing tests `autorepeat_activated_yields_a_single_press`/`toggle_hold_does_not_stop_the_session` already prove this — the `Dedup` state machine is boolean-flag-based, not counter-based, so it's provably burst-size-independent. Added one defensive/forward-looking test, `large_autorepeat_burst_still_yields_a_single_toggle_edge` (200-repeat burst), as a regression guard against a future counter-based refactor; no functional gap was found or closed.)*
- [X] T048 [P] [US2] Hermetic test: every user-facing acknowledgement action currently exposed only via a pointer/hover path (e.g. the notification dismiss (×) if present) has a non-pointer equivalent (keyboard activation or auto-timeout) asserted against the indicator's action list (FR-019). *(Genuine gap found: no test proved that starting a new session clears ANY held notice regardless of severity. Extracted the previously-inline decision in `hud.js`'s `_applyDescriptor` into a new pure function `nextHeldNotice(currentHeld, descriptor)` in `hudLogic.js`, test-first — confirmed red (`nextHeldNotice` didn't exist) before implementing. Code-reviewed: refactor is behaviorally identical to the original inline logic, verified by hand-tracing every severity×held combination plus the full test suite.)*

### Implementation for User Story 2

- [X] T049 [US2] Implement autorepeat de-duplication in `client/myna-desktop/src/shortcut/mod.rs` (a debounce/edge-detection window). Satisfies T047. *(No-op: T047's audit found the existing `Dedup` logic already correct; no new de-dup logic was needed.)*
- [X] T050 [US2] Audit and, where needed, add a non-pointer path (e.g. an explicit key binding or timeout) for any indicator acknowledgement action found pointer-only in T048; wire through `indicator/{dbus,gtk,notify}.rs`. *(Audit: the HUD's `(×)` dismiss button is the only pointer-only affordance; `NotifyIndicator` provides no dismiss action of its own (relies on the notification-center's own keyboard support). Judged FR-019-satisfied via the T048 behavior — documented at the dismiss-button's construction site in `hud.js` and in `notify.rs`'s module doc. **Code review (2026-08-27) found one real edge case this doesn't fully cover**: a repeated, byte-identical critical error (pre-capture failures specifically, e.g. `abort_before_capture`) is not perceivably re-announced due to `DbusIndicator::publish`'s per-wire-state dedup — tracked as `docs/project-plan.md` T79 rather than fixed here, since a correct fix touches feature 004's shared C2 dedup contract and is out of scope for this task; documented as a known limitation in the dismiss-button comment.)*
- [X] T051 [US2] [P] Add/confirm a manual verification note in `quickstart.md` Scenario 4 covering sticky-keys/slow-keys behavior specifically (cannot be simulated hermetically — GNOME accessibility features live in the compositor/input stack, not `myna-desktop`).
- [X] T052 [US2] [P] Confirm (test if feasible, else document as manual) that activating dictation via the desktop's own switch-access/dwell-click produces an ordinary key/pointer event indistinguishable from a normal shortcut activation — no Myna-side accommodation needed (FR-020). Record the finding in `quickstart.md`. *(Confirmed by reading `portal.rs`'s `bind()` and `control.rs` in full: neither has any concept of "input method" — the portal path only sees bus-level `Activated`/`Deactivated`, the control-socket path only sees a Unix connect. Recorded in quickstart.md Scenario 3.)*

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

- [X] T053 [P] [US3] Hermetic test: the coverage-matrix gate (T020–T027, already passing) is re-run as part of this story's CI-facing suite to confirm SC-002 end-to-end (no new logic — a regression-gate placement task, not new production code). *(Confirmed: `cargo test -p myna-desktop --lib coverage` — 9/9 passing, unchanged from the Foundational phase.)*
- [X] T054 [P] [US3] Hermetic test in `client/myna-desktop/src/sound/mod.rs`: a `SoundCue` player exposes `play(cue: CueKind)` behind a `Preferences::sound_cues_enabled()`/per-cue-toggle gate, using a fake PipeWire sink for the hermetic tier. **Write first, observe fail, then implement** *(`CueKind`, `SoundCuePlayer` trait, `NullSoundCuePlayer`, `FakeSoundCuePlayer`, `GatedSoundCuePlayer<S,P>` — gates on both the global `sound_cues_enabled()` and a new `Preferences::cue_enabled(cue)` default-`true` seam. Tests: `play_is_recorded_when_sound_cues_are_enabled`, `play_is_a_silent_noop_when_sound_cues_are_disabled`.)*
- [X] T055 [P] [US3] Env-gated integration test `client/myna-desktop/tests/sound_hw.rs` (`MYNA_PIPEWIRE_TESTS=1`): a real PipeWire playback stream plays a short embedded cue clip without error. *(Verified passing against a real PipeWire graph on this machine: `MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-desktop --test sound_hw` — `real_pipewire_plays_every_cue_without_error` ok.)*
- [X] T056 [P] [US3] Hermetic test: cue playback never blocks or delays the capture path — asserted by timing the controller's transition handling with sound cues enabled vs. disabled and asserting no measurable difference (FR-011 non-blocking half; the WER half is measured separately, T060). *(`tests/accessibility.rs`'s `sound_cue_wiring_adds_no_measurable_delay_to_the_capture_path`: runs a full utterance with `NullSoundCuePlayer` vs. `FakeSoundCuePlayer`, asserts the wall-clock delta is under a 50ms tolerance. Structural guarantee: `SoundCuePlayer::play` is a plain, non-`async fn`, so `controller.rs` cannot `.await` it even by accident.)*

### Implementation for User Story 3

- [X] T057 [US3] Implement `SoundCue`/playback in `client/myna-desktop/src/sound/mod.rs`, reusing `myna-audio`'s vendored `pipewire` crate for an output stream (research.md R2). Satisfies T054/T055. *(`sound/playback.rs`'s `PipeWireSoundCuePlayer`: three fixed, mutually-distinct sine-tone clips (880Hz/660Hz single-tone for Start/End, 220Hz double-pulse for Failure), played on a fresh short-lived `Direction::Output` stream per cue, fire-and-forget on a dedicated `myna-pw-cue` thread with a `MAX_CLIP_THREAD_LIFETIME` watchdog timer mirroring `native.rs`'s capture-side pattern. Both the tone synthesis (4 hermetic tests) and the real stream connect/playback (T055's env-gated test) are verified.)*
- [X] T058 [US3] Wire `controller.rs` to trigger `SoundCue::play()` on session start/end/failure, decoupled from (never blocking) the capture/announcement path. Satisfies T056. *(`DesktopController` gained an optional `sound: Box<dyn SoundCuePlayer>` field (default `NullSoundCuePlayer`, opt-in via `.sound(...)` on the builder — every pre-existing call site is unaffected). `play(CueKind::SessionStart)` on the Recording transition; `play(CueKind::SessionEnd)` on a successful `Completed` outcome; `play(CueKind::Failure)` on `abort_before_capture`, `SessionOutcome::Failed`, and the backend `Err` path. Deliberately silent on `Cancelled`/`Aborted` (neither success nor failure). Tests: `a_successful_utterance_plays_session_start_then_session_end`, `a_failed_utterance_plays_session_start_then_failure_not_session_end`.)*
- [X] T059 [US3] [P] Confirm/extend `extensions/myna-shell/accent.js`'s existing reduced-motion/high-contrast GSettings reads (feature 004) cover FR-012's full list (text-scale, forced-colours) — add any missing read, with a GJS contract test for the fallback behavior. *(Added `text-scaling-factor` (same `org.gnome.desktop.interface` schema) and `high-contrast` (`org.gnome.desktop.a11y.interface` — the real GNOME 47+ key, verified via `gsettings list-keys` against a running session; GNOME has no separate "forced colours" toggle, so this is also FR-012's forced-colours read). `resolveTextScale`/`resolveHighContrast` pure resolvers + `SystemPreferences.textScale`/`.highContrast` live getters, each schema-existence-guarded and defaulting safely (1.0 / false) when absent. 12 new assertions in `test/accent.test.js`, all passing; full `test/run-suite.sh` shows no new failures (only the pre-existing, environment-specific `compat-probe.sh` reduced-motion failure, confirmed identical on a clean `origin/integration-220627` worktree).)*
- [X] T060 [US3] Run the real-corpus WER benchmark (`dev/fetch_real_corpus.py` family) with sound cues + announcements enabled vs. a silent baseline; record the delta as a checked-in watermark, asserting ≤0.5pp (SC-006, constitution Principle III). Document the run in `quickstart.md` Scenario 6. *(Judgement call, documented in quickstart.md: `dev/bench.py`'s corpus harness feeds pre-recorded clips directly to the ASR socket and never opens a live microphone or speaker, so it cannot exercise — or regress-test — the only path by which a sound cue could physically affect a transcript (acoustic leakage from speaker back into an open mic). Recording a WER delta from that harness would be fabricated, not measured. Documented instead: the architectural reason automation doesn't apply, plus a live-hardware acoustic manual protocol as the real check, matching the `docs/project-plan.md` T77 precedent for lab-only work that isn't corpus-automatable.)*
- [X] T061 [US3] [P] Manual verification pass (recorded in `quickstart.md` Scenario 5) at 200% text scale, high-contrast theme, forced colours, and reduced motion — confirm no clipping/truncation and the reduced-motion static equivalent for live capture. *(quickstart.md Scenario 5 rewritten with concrete `gsettings set` commands for each of the four settings (200% `text-scaling-factor`, `org.gnome.desktop.a11y.interface high-contrast` — also GNOME's forced-colours equivalent, `enable-animations false`), what to observe at each state, and a reset step.)*

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

- [X] T062 [P] [US4] Hermetic test in `client/myna-desktop/tests/failure.rs`: every known failure source today (`controller.rs`'s `OrchestratorEvent::Error` messages, `inject::InjectError`'s `SecureField`/`NoTarget`/`Unavailable`/`Backend` variants) has a registered `FailurePresentation` with a non-empty, jargon-free `message` (F1, FR-023). **Write first, observe fail, then implement** *(Landed in `client/myna-core/src/failure.rs` (moved from the `myna-desktop` scaffold — see T067's note) as `every_known_failure_source_has_a_non_empty_plain_language_presentation`, covering all 15 registered ids including the `BackendError`/wire-code and `MODEL_LOAD_SLOW` entries added alongside `InjectError`'s four.)*
- [X] T063 [P] [US4] Hermetic test: a single shared fixture renders the same `FailurePresentation` through the indicator-error path, the `notify-rust` toast path, `myna-cli`'s stderr, and the announcer, and asserts identical `message`/`recovery_action` text across all four (F2, FR-024). *(`indicator::notify::tests::a_presentation_backed_error_toast_carries_the_same_wording_the_announcer_speaks` covers indicator+toast+announcer in one process; `myna-cli`'s `tests::render_failure_matches_failure_presentation_render_exactly` covers the terminal surface against the same `FailurePresentation::render` all three others call — one shared formatting function, not independently-authored wording, is what makes this hold across a process boundary.)*
- [X] T064 [P] [US4] Hermetic test: two different call sites referencing the same failure `id` never diverge in wording (F3, FR-024a). *(`myna-core`'s `two_call_sites_naming_the_same_id_never_diverge_in_wording` + `the_shared_default_registry_matches_a_fresh_default_registry_for_every_known_id`.)*
- [X] T065 [P] [US4] Hermetic test: `Critical` presentations remain queryable/persistent until acknowledged; `Recoverable` ones auto-dismiss but are still retrievable afterward via a small in-memory "last notice" accessor (F4, FR-025/026). *(`tests/controller.rs`'s `last_notice_is_none_before_any_failure`, `last_notice_records_a_critical_failure_and_it_stays_queryable`, `last_notice_records_a_recoverable_model_load_slow_notice_after_auto_dismiss` — the last one confirms the notice is still retrievable even after the indicator itself has returned to `Hidden`.)*
- [X] T066 [P] [US4] Hermetic test with a fake clock: a long-running operation (model loading) past a defined threshold produces an actionable `FailurePresentation`-shaped message distinguishable from ordinary periodic progress (F5, FR-027). *(Pure predicate `loading_exceeds_threshold` unit-tested with plain `Duration` values (`controller::tests::loading_exceeds_threshold_is_{false_before,true_at_and_past}_the_threshold`); the real-timeline behavior is covered by `tests/controller.rs`'s `#[tokio::test(start_paused = true)]` tests `a_model_load_past_the_threshold_surfaces_an_actionable_notice`/`a_model_load_under_the_threshold_never_surfaces_a_notice`. **Scope decision, not a gap**: FR-027 also describes a *periodic* progress ping before the threshold — not implemented, because `accessibility::AnnouncingIndicator`'s intentional same-state dedup (added for US1) would silently swallow a repeat of an unchanged state and has no "repeat this on purpose" escape hatch today; documented in `controller.rs`'s module comment and `docs/project-plan.md` as a follow-up rather than worked around here.)*

### Implementation for User Story 4

- [X] T067 [US4] Populate the `FailurePresentation` registry in `client/myna-desktop/src/failure.rs` with one entry per known failure source. Satisfies T062. *(Moved the registry to `client/myna-core/src/failure.rs` instead of leaving it in `myna-desktop` — `myna-cli` (T071) needs the identical presentations and depends on `myna-core` but not `myna-desktop`; `myna-desktop::failure`/`accessibility::Severity` are now thin re-exports so no in-crate call site had to change its import path. 15 entries: `SECURE_FIELD`/`NO_TARGET`/`INJECTION_UNAVAILABLE`/`INJECTION_BACKEND_ERROR`/`TARGET_CLOSED` (closed-set ids), `BACKEND_CONNECT`/`BACKEND_HANDSHAKE`/`BACKEND_WIRE`/`BACKEND_CLOSED`/`BACKEND_TRANSPORT`/`UNKNOWN_BACKEND_FAILURE` (closed-set ids for `BackendError`), `CODE_INTERNAL`/`CODE_CONNECTION_CLOSED`/`CODE_INFERENCE_FAILED`/`CODE_CAPTURE_FAILED` (open-ended wire codes, registered under an id equal to the code itself via `lookup_by_code`), and `MODEL_LOAD_SLOW` (T066/T070). Added `FailureRegistry::spoken`/`spoken_by_code` (a precomputed, leaked `"{message} {recovery_action}"` per id) and `FailurePresentation::render(detail)` — both needed because `AnnouncementText` accepts only `&'static str` and `myna-cli`/`IndicatorState::from_failure` both need the identical combined formatting, respectively.)*
- [X] T068 [US4] Route `controller.rs`'s error handling, `indicator/notify.rs`'s toast text, and the announcer's failure announcements all through `failure::lookup(id)` rather than ad-hoc strings. Satisfies T063/T064. *(`IndicatorState::Error` gained a `presentation: Option<&'static FailurePresentation>` field + a `from_failure(presentation, detail)` constructor; `controller.rs`'s `abort_before_capture`, the cancelled/`TARGET_CLOSED` branch, `SessionOutcome::Failed` (via `lookup_by_code`), and the backend `Err` branch (via a new shared `myna_orchestrator::backend_error_presentation`, authored once next to `BackendError` itself so `myna-desktop` and `myna-cli` cannot independently diverge) all route through the registry. `accessibility::format::format_state_announcement` speaks `presentation`'s combined `spoken()` text when present, falling back to the previous generic "Notice"/"Error" wording for the handful of ad-hoc recoverable notices this contract doesn't cover ("No speech detected"/"Focus lost").)*
- [X] T069 [US4] Implement the "last notice" accessor + critical-persistence/recoverable-auto-dismiss timing in `controller.rs`. Satisfies T065. *(`DesktopController` gained `last_notice: Option<&'static FailurePresentation>` + a `pub fn last_notice(&self)` getter, set by the new `report_failure` method (replacing the old ad-hoc `report_critical` free function) and by the model-load-slow watchdog path. Critical-persistence itself is unchanged, existing behavior (the indicator/wire dedup layer, feature 004 contract C2); this task's actual new surface is the retrieval-after-dismiss half.)*
- [X] T070 [US4] Implement the long-operation progress/threshold logic (periodic non-visual progress indication + actionable message past threshold) in `controller.rs`, reusing the announcer for the periodic pulse. Satisfies T066. *(A one-shot `tokio::time::sleep_until` branch added to the utterance's `select!` loop, armed only while a `Loading` window is open (tracked via `loading_since: Option<Instant>`, cleared on `Ready`/`Done`/`Error` and also the moment capture stops — `Release`/`FocusOut`/`TargetGone`/trigger-ended — since "still loading" is moot once the user isn't holding the key anymore). See T066's note for the periodic-ping half's scope decision.)*
- [X] T071 [US4] [P] Wire `client/myna-cli/src/main.rs` to render failures via `failure::lookup(id)` on stderr (shared with US5's terminal-output work — coordinate, don't duplicate). *(Both `dictate_clips` and `dictate_mic`'s `SessionOutcome::Failed`/`Err(BackendError)` arms now render via a new `render_failure` helper calling the shared `FailurePresentation::render`; replaced the old "✗ ..." ad-hoc lines and the emoji-prefixed capture_failed special case (redundant now that `CODE_CAPTURE_FAILED`'s registry entry already carries mic-specific wording) with a plain `[error]`-prefixed line — no ANSI/emoji, coordinating with (not duplicating) US5's still-to-come terminal-output work.)*

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

- [X] T072 [P] [US5] Hermetic test in `client/myna-cli/tests/output.rs`: with colour disabled, every state transition and result line contains an explicit textual marker (e.g. `[listening]`) and no ANSI colour codes (T1, FR-028). **Write first, observe fail, then implement** *(`myna-cli` is a `[[bin]]`-only crate with no library target, so the test exercises the pure `myna_orchestrator::render_event_line` function `StdoutSink` is a thin I/O wrapper over, rather than capturing real stdout — same pure/IO split this codebase uses throughout. `every_state_transition_and_result_line_has_an_explicit_textual_marker` + `markers_are_unique_per_distinct_kind_of_event` cover all 8 `OrchestratorEvent` variants that produce output.)*
- [X] T073 [P] [US5] Hermetic test: every stdout write ends in `\n` and contains no carriage-return/cursor-movement ANSI sequences across a simulated multi-transition session (T2, FR-029). *(`a_full_simulated_session_never_contains_a_carriage_return_or_ansi_sequence` runs a 7-event script through `render_event_line` asserting no `\r`/`\x1b` in any line; `println!`/`eprintln!` in `StdoutSink::emit` always terminate in `\n` by construction. `error_and_dropped_audio_route_to_stderr_everything_else_to_stdout` confirms the stream split.)*
- [X] T074 [P] [US5] Hermetic test: a forced failure is written to stderr using the exact `FailurePresentation` text from `failure::lookup` (T3, FR-029) — shares the fixture from T063. *(`a_mid_stream_error_uses_the_exact_registered_failure_presentation_text` asserts the rendered line equals `format!("[error] {}", presentation.render(detail))` exactly — not a coincidentally-matching substring; `an_unrecognized_error_code_falls_back_to_the_shared_unknown_failure_presentation` covers the open-ended-wire-code fallback.)*

### Implementation for User Story 5

- [X] T075 [US5] Implement colour/emoji stripping + explicit textual state markers in `client/myna-cli/src/main.rs`. Satisfies T072. *(Landed in `client/myna-orchestrator/src/sink.rs` (not `main.rs` directly — `StdoutSink` is defined there and `myna-cli` uses it unmodified): a new pure `render_event_line`/`RenderedLine`/`OutputStream` replace the old ad-hoc `println!`/`eprintln!` calls, each line now prefixed with an explicit `[marker]` (`[loading]`/`[ready]`/`[progress]`/`[committed]`/`[partial]`/`[done]`/`[error]`/`[dropped-audio]`) alongside (not instead of) the existing decorative emoji — FR-028 requires meaning to survive emoji *removal*, not that emoji be stripped by default, so both coexist. The mid-stream `OrchestratorEvent::Error` arm now also routes through `myna_core::failure::lookup_by_code`, matching T077's terminal-outcome path instead of an independently-worded ad-hoc string.)*
- [X] T076 [US5] Remove/replace any in-place redraw or spinner-animation output in `client/myna-cli/src/main.rs` with append-only line output. Satisfies T073. *(The one genuine in-place-redraw surface is `main.rs`'s live VU meter (`--mic`'s `render_meter`, `\r`-overwritten). Rather than convert it to append-only lines (audio stats update many times/second — a line per update would itself spam a screen reader with far more frequent re-reads than the meter is worth), added `myna_core::term::plain_output()` (the `NO_COLOR` convention, quickstart.md Scenario 8) and gated the meter's draw, its line-clear, and `MicMeterSink`'s pre-event clear on `!plain_output()` — under `NO_COLOR=1` the meter is silently suppressed (channel still drained so the sender never blocks) rather than adapted; sighted default behavior is unchanged. `myna_core::term`'s `plain_output_requested(no_color: Option<&str>)` is the hermetically-tested pure predicate; `plain_output()` is the thin env-reading wrapper.)*
- [X] T077 [US5] Wire failure rendering to `failure::lookup` (shared with T071). Satisfies T074. *(Already wired by T071's `render_failure` helper in `main.rs` for the terminal `SessionOutcome::Failed`/`BackendError` outcome path; this task's remaining piece — the mid-stream `OrchestratorEvent::Error` event path in `sink.rs` — is what T075's note above describes.)*

**Checkpoint**: All five user stories are independently functional.

---

## Phase 8: Polish & Cross-Cutting Concerns

- [ ] T078 [P] Run `quickstart.md` Scenarios 1–8 manually against a real GNOME session (dev build) and record results/gaps directly in `quickstart.md`. *(NOT DONE — requires a real GNOME desktop session; this working environment is a headless/sandboxed dev container (AppArmor blocks even the accessibility-bus connection — see `docs/project-plan.md` T56's verification note), so this remains a genuine manual step for whoever has a GNOME session available.)*
- [ ] T079 [P] Repeat the subset of `quickstart.md` scenarios that can run against the packaged, strictly confined `myna` snap (FR-033/SC-009); record any confinement-specific finding (e.g. `desktop` plug sufficiency for `org.a11y.Bus`). *(NOT DONE — requires building/installing the packaged snap and a real desktop session, neither available in this working environment.)*
- [X] T080 [P] Update `docs/desktop-injection.md` / `docs/project-plan.md` to reflect the new `accessibility`/`sound`/`failure`/`coverage` modules and close out T56; annotate T31/T54/T58/T59/T61/T62 with this feature's overlap resolution (per spec Assumptions). *(`docs/desktop-injection.md` gained an "Accessibility" section summarizing the four new seams and how `myna-cli` shares the `failure` registry across the crate boundary. `docs/project-plan.md`: T56 closed out (done, feature 011) with a summary across all five user stories; T58/T59/T62/T31/T54 each annotated with what this feature did and did NOT resolve for them (verified via direct code inspection — e.g. T59 is now genuinely resolved upstream rather than left open: the rebase onto `integration-220627` brought `myna_desktop::AutoStop`, which consumes the `silence-timeout` key end to end, superseding this feature's persisted-but-unconsumed `silence_auto_stop_seconds` preference).)*
- [ ] T081 [P] Add CI wiring (if not automatic via existing `cargo test`/`workshop run` targets) to ensure `MYNA_ATSPI_TESTS`/`MYNA_PIPEWIRE_TESTS`-gated suites and the GJS coverage/contrast tests run on every PR (constitution Development Workflow gate). *(Partially done. The GJS coverage/contrast tests (`extensions/myna-shell/test/coverage.test.js`, `accent.test.js`) already run on every PR — they're part of `make test-extension` → `.workshop/myna-shell.yaml`'s `gjs-test`, no new wiring needed. `MYNA_PIPEWIRE_TESTS`: added the new `sound_hw` suite to `.workshop/myna.yaml`'s existing `test-gated` action alongside `pipewire_hw` (same virtual PipeWire graph, verified working: `cargo test -p myna-audio --test pipewire_hw -p myna-desktop --test sound_hw` runs both correctly). **Two things NOT done, on purpose, needing a decision rather than a silent choice:** (1) `MYNA_ATSPI_TESTS`'s `atspi_hw` suite is NOT added to `test-gated` — `dev/gated-tests.sh` stands up a scratch D-Bus session bus/IBus/PipeWire but no `org.a11y.Bus`, and extending it to do so (and confirming it actually works) is unstarted, unverifiable work in this sandboxed environment. (2) `test-gated` itself is NOT invoked from any CI workflow today — `.github/workflows/ci.yml`'s `workshop`/`extension` jobs call `make test-client`/`make test-extension` only, which run the hermetic suite (gated tests skip cleanly); this is a **pre-existing** gap that predates this feature (the existing `ibus_hw`/`dbus_hw`/`pipewire_hw` gated suites aren't in CI either), so silently adding a new CI job that stands up real D-Bus/IBus/PipeWire/AT-SPI services on hosted GitHub runners is an infrastructure decision for the team, not something to decide unilaterally inside this feature branch.)*
- [X] T082 Full regression pass: `make test-client`, `make test-extension`, and the gated suites all green together. *(`make test-client` (`cargo test --workspace`): all green throughout US1-US5's development, re-confirmed after every change. `make test-extension` (GJS + headless-Shell suites): all green, including the previously-failing `compat-probe.js` reduced-motion check, which was fixed upstream on `integration-220627` mid-session (see repo memory) and inherited by rebasing. Gated suites (`workshop run myna test-gated`): NOT run in this pass — `workshop launch`/`workshop run` requires a Workshop-managed environment (containers/VMs) not available in this direct-terminal working session; the equivalent commands were run directly instead (`cargo test -p myna-audio --test pipewire_hw`, `MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-desktop --test sound_hw`), both green.)*


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

---

## Phase 9: Convergence

Remaining work found by assessing the codebase against spec.md, plan.md, and
the tasks above. Two findings are consequences of the rebase onto
`integration-220627`, which replaced the GJS HUD (`hud.js`, `states.js`,
`accent.js`, `stylesheet.css`) with the `myna-hud` renderer the Shell extension
now merely hosts: FR-006's "both indicators identically" is satisfied by there
being one renderer, but a gate and a comment were left pointing at files that
no longer exist.

*`integration-220627` is named throughout this file because it was this
branch's base while the work was done. It has since been merged and deleted;
everything attributed to it below is now simply in `main`, and this branch was
rebased onto `main` directly.*

- [X] T083 Measure the sound-cue/announcement WER delta against a silent baseline on real hardware and check the result in as a baseline with its declared tolerance — or, if the acoustic path genuinely cannot be measured, re-ratify SC-006's watermark as explicitly unmeasurable so the gap is a recorded decision rather than an unmet MUST, per Constitution III and SC-006 (missing) — CRITICAL
- [X] T084 Re-point the contrast regression gate at the shipped stylesheet: `client/myna-desktop/src/coverage.rs`'s `stylesheet_colours` pins its pairs to the deleted `extensions/myna-shell/stylesheet.css`, while the HUD now ships `client/myna-hud/src/style.css` with different values, so the gate no longer guards any rendered colour, per FR-013/FR-032/SC-005 (contradicts)
- [X] T085 Make the impending silence auto-stop perceivable before it fires — `client/myna-desktop/src/controller.rs`'s `auto_stop_due` branch logs and stops with no prior signal on any channel, per FR-018 and US2/AC4 (missing)
- [ ] T086 Run `quickstart.md` Scenarios 1–8 against a real GNOME session and record the results and any gaps in `quickstart.md`, per FR-031 and SC-001/SC-005/SC-007 (missing) — completes T078
- [ ] T087 Run the packaged subset of `quickstart.md` against the strictly confined `myna` snap (now buildable via `make snap-myna`) and record any confinement-specific finding, per FR-033 and SC-009 (missing) — completes T079. *(Its headline open question — whether the `desktop` plug alone reaches `org.a11y.Bus` — is now answered; see "Confinement: what the packaged build proved" below. What remains is the human half: exercising the scenarios on a real confined install.)*
- [X] T088 Emit the periodic "work is still progressing" indication during long operations, which needs a deliberate-repeat path through `accessibility::AnnouncingIndicator`'s same-state dedup; the past-threshold actionable message (T070) already lands, per FR-027 (partial)
- [X] T089 Re-announce a repeated identical critical failure that `DbusIndicator::publish`'s per-wire-state dedup currently swallows, so a non-visual user perceives the second occurrence, per FR-024/FR-025 and `docs/project-plan.md` T79 (partial)
- [X] T090 Stand up an `org.a11y.Bus` in `dev/gated-tests.sh` and enrol the `MYNA_ATSPI_TESTS`-gated `atspi_hw` suite in `test-gated`, which now reaches CI through `make test-client`, per FR-030 and Constitution IV (partial) — completes T081
- [X] T091 Correct the stale comment at `extensions/myna-shell/host.js`'s proxy-presence block, which still describes "both the host and the announcer" reading one proxy after the GJS announcer was removed (contradicts)
- [X] T092 Justify or hand off the `myna-config` SwitchRow rendering added on this branch: spec Assumptions place the settings UI with a separate team, so this surface is outside the feature's stated scope even though the accessibility keys need somewhere to appear (unrequested)

### Phase 9 outcomes

Eight of the ten landed as code or as a recorded decision. Two did not, and
are not "nearly done" — they need hardware this branch cannot reach:

- **T086** needs a real GNOME session with a running screen reader. It cannot
  be run from a headless or sandboxed environment: the AppArmor confinement in
  some dev sandboxes blocks `org.a11y.Bus` outright, and Scenarios 1–8 turn on
  what a user *hears and sees*, which no automated harness substitutes for.
  Everything that can be checked without a session already is — `atspi_hw` now
  runs against a real accessibility bus in CI (T090), so the bus protocol
  itself is covered; what remains is the human half.
- **T087** needs the strictly confined `myna` snap installed on that same
  desktop. `make snap-myna` builds it, but installing and exercising a
  confined snap is a privileged operation on a real machine. Its open question
  — whether snapd's `desktop` plug alone reaches `org.a11y.Bus`, or whether an
  extra interface connection is needed — has since been answered against a real
  confined install; see "Confinement: what the packaged build proved" below.
  The scenario runs themselves remain outstanding.

Both are left unticked deliberately. Ticking them on the strength of the
automated coverage would misrepresent what has been verified — SC-001, SC-005,
SC-007, and SC-009 all rest on these runs.

Resolutions worth carrying forward:

- **T083** did not produce a watermark, because none can be produced honestly:
  the only way a sound cue can affect a transcript is acoustic, and neither CI
  nor the corpus harness has an acoustic path. Re-ratified as an exempt
  live-hardware check in plan.md's post-design re-check, with the reasoning,
  rather than left as an unmet MUST.
- **T088**'s repeat carries the elapsed time, so each progress indication
  genuinely differs. No "repeat this on purpose" escape hatch was added to the
  `Indicator` seam: the dedup keeps its guarantee for every other surface.
- **T089** exempts only critical failures from the dedup, at both the announcer
  and the D-Bus publisher. Safe because `completion_indicator_state` — the
  deliberate double-call the dedup exists for — never yields one.

### Confinement: what the packaged build proved

T087's headline question was whether the `desktop` plug alone reaches
`org.a11y.Bus`. Probed from inside the installed, strictly confined snap
(`snap run --shell myna.myna`), the answer is two-part, and only the first part
was fixable here.

**Reaching the bus: yes, once the bootstrap stops reading properties.** The
plug does allow the connection, but the `atspi` crate's
`AccessibilityConnection::new()` builds a `RegistryProxy` and a
`zbus::fdo::DBusProxy`, and zbus reads `Properties.GetAll` when a proxy is
constructed. Under confinement that read is permitted only on
`/org/a11y/atspi/accessible/[0-9]*`, so all three bootstrap paths are refused:

| path | interface | result |
|---|---|---|
| `/org/freedesktop/DBus` | `org.freedesktop.DBus` | `AccessDenied` |
| `/org/a11y/atspi/registry` | `org.a11y.atspi.Registry` | `AccessDenied` |
| `/org/a11y/atspi/accessible/root` | `org.a11y.atspi.Accessible` | `AccessDenied` |

Resolving the bus address and connecting to it directly stays inside what the
profile permits. That is the shipped code path now, and
`client/myna-desktop/tests/atspi_confined.rs` pins it against a bus whose
policy mirrors these denials, so the old bootstrap cannot come back unnoticed.

**Delivering the announcement: no, and not by anything this repository can
change.** snapd's `desktop-legacy` interface allowlists AT-SPI signal members
by name — `ChildrenChanged`, `PropertyChange`, `StateChanged`,
`TextCaretMoved`. `Announcement` is not among them, so the signal is dropped:

```
apparmor="DENIED" operation="dbus_signal" bus="accessibility"
  path="/org/a11y/atspi/accessible/root" interface="org.a11y.atspi.Event.Object"
  member="Announcement" mask="send" label="snap.myna.myna" peer_label="unconfined"
```

An A/B against an unconfined subscriber confirms the allowlist is the whole
story: `ChildrenChanged` is delivered, `Announcement` is denied, same process,
same bus, same instant. No combination of currently-available plugs helps,
because the member is not allowlisted anywhere; adding it to `desktop-legacy`
is a snapd-side change.

**Consequence for the requirements.** FR-007's "MUST function in the strictly
confined shipped package" and FR-033 are therefore met for everything except
the announcement itself, which is the feature's centre. Unconfined builds —
including every development run and the whole `atspi_hw`/`atspi_confined`
suite — are unaffected. This is a categorical limitation of strict confinement
today rather than anything specific to Myna: no strictly confined application
can announce.

Two measurement traps are worth recording, because each produces a confident
wrong answer. `dbus-monitor` is not a policy oracle — a `BecomeMonitor` client
receives traffic the policy would have dropped, so a denied signal still
appears. And the subscriber must be genuinely unconfined: the profile rules end
`peer=(label=unconfined)`, so a process started from an editor's terminal may
carry a non-`unconfined` label and see *every* member denied, which reads as
the entire allowlist being dead.
