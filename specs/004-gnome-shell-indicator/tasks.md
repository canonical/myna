# Tasks: GNOME Shell Extension for Myna Dictation UI

**Input**: Design documents from `/specs/004-gnome-shell-indicator/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Two tiers (plan Constitution Check).
- The **Rust D-Bus publisher** in `myna-desktop` is a *shipped system component*:
  constitution Principle I (Red-Green TDD) applies in full — every behavior-bearing
  task is preceded by a failing hermetic test over a **fake `Bus` seam** (no session
  bus), and real session-bus behavior is proven by an env-gated suite
  (`MYNA_DBUS_TESTS=1`) runnable identically on the desktop VM and on hardware
  (Principle II).
- The **GJS extension** is *evaluation-harness-tier* (plan Complexity Tracking):
  its pure logic (`states.js`/`vumeter.js` + stub-proxy lifecycle) gets GJS contract
  tests, but the compositor/animation/focus behavior is proven by a **manual
  on-hardware acceptance** (quickstart §5), not test-first.

**Organization**: Tasks grouped by user story (US1–US4) for independent
implementation and testing. Priority order: US1 (P1 🎯 MVP) + US2 (P1, co-MVP) →
US3 (P2) → US4 (P3). US1 and US2 are co-P1 (a focus-safe overlay that is also
state-legible is the MVP); they share the publisher+extension skeleton built in
Foundational, then split into focus-safety (US1) and state-treatments (US2).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1–US4 for story-phase tasks; Setup/Foundational/Polish carry none
- All paths are repo-relative

## Path Conventions

- Rust publisher (shipped): `client/myna-desktop/src/{dbus/mod.rs,indicator/dbus.rs,shortcut/dbus.rs,bin/myna-desktop.rs}`
- Reused seams (unchanged): `client/myna-orchestrator/src/{trigger,sink}.rs`, `client/myna-desktop/src/{indicator/mod.rs,shortcut/mod.rs,controller.rs}`
- Reused levels (unchanged): `client/myna-audio` (`CaptureSource::stats()` → `watch::Receiver<AudioStats>`)
- Hermetic publisher tests: `client/myna-desktop/tests/dbus_indicator.rs` + `#[cfg(test)]` in the modules
- Env-gated publisher suite: `client/myna-desktop/tests/dbus_hw.rs` (gate: `MYNA_DBUS_TESTS=1`, via `dbus-run-session`)
- GJS extension bundle: `extensions/myna-shell/` (`metadata.json`, `extension.js`, `dbus.js`, `indicator.js`, `states.js`, `vumeter.js`, `stylesheet.css`, `test/states.test.js`)
- Shared contract: `specs/004-gnome-shell-indicator/contracts/dbus-interface.md` (`org.myna.Dictation`)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: environment + module/bundle scaffolding so both halves build and the
D-Bus contract has a home on each side.

- [X] T001 Extend the Workshop definition (constitution Principle IV) in `.workshop/myna.yaml` (and the `desktop` SDK hooks) to add this feature's deps: a **session D-Bus** for the gated publisher suite (`dbus-run-session`; likely already present via the feature-003 desktop SDK — confirm) and, for the extension acceptance, **`gjs`** + a **`gnome-shell`** session (GNOME 50/51). Validate `workshop launch myna` still builds and `gjs --version` / `dbus-run-session --version` resolve (plan Complexity Tracking Workshop row)
- [X] T002 Scaffold the Rust publisher modules in `client/myna-desktop/src/`: create `dbus/mod.rs` (declare `pub struct DictationService`, a `Bus` seam trait, and the served `org.myna.Dictation` skeleton), `indicator/dbus.rs` (declare `pub struct DbusIndicator`), `shortcut/dbus.rs` (declare `pub struct DbusTrigger`); wire `mod dbus;` + `pub mod` entries in `lib.rs` and the `indicator`/`shortcut` mod files. No new crate, no new dependency (reuse vendored `zbus`). Confirm `cargo build -p myna-desktop` and `--no-default-features` both link with the stubs
- [X] T003 [P] Create the env-gated test file `client/myna-desktop/tests/dbus_hw.rs` with a `MYNA_DBUS_TESTS` gate helper that skips cleanly when unset (compiles/runs as a no-op offline), mirroring feature-003's gate style
- [X] T004 [P] Scaffold the GJS bundle `extensions/myna-shell/`: `metadata.json` (`uuid` `myna-shell@myna.dev`, `shell-version: ["50","51"]`, name/description, no settings schema — Out of Scope), empty ESM modules `extension.js`/`dbus.js`/`indicator.js`/`states.js`/`vumeter.js`, `stylesheet.css`, and `test/states.test.js`. Confirm `gjs -m -c "import('./extensions/myna-shell/states.js')"` (or equivalent import smoke) loads without syntax error

**Checkpoint**: workspace builds with the publisher stubs under both feature settings; the gated suite compiles as a no-op offline; the GJS bundle parses; existing suites still green.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the shared `org.myna.Dictation` contract encoded on both sides — the
`Bus` seam + fake bus (Rust) and the pure `states.js` mapping (GJS) — that ALL
stories depend on. No user story can start until this is done.

**⚠️ CRITICAL**: complete before ANY user story phase.

- [X] T005 Hermetic test in `client/myna-desktop/tests/dbus_indicator.rs` (or `#[cfg(test)]` in `dbus/mod.rs`) for the `Bus` seam + a `FakeBus` recording emitted `StateChanged` signals and property sets. Assert the fake records signal name + args and the latest property snapshot. **Write first, observe fail, then satisfy with T006**
- [X] T006 Implement the `Bus` seam trait + `FakeBus` in `client/myna-desktop/src/dbus/mod.rs` (records `emit_state_changed(state, reason)` and `set_property(name, value)`); the real `zbus`-backed impl is later (T024). Satisfies T005 (contract dbus-interface.md, publisher.md P-seam)
- [X] T007 Hermetic test for the `IndicatorState → State`-string mapping (data-model E1 table incl. the `loading`/`recording` split, R4/C5) in `client/myna-desktop/src/indicator/dbus.rs` `#[cfg(test)]`: `Hidden→idle`, `Recording→recording`, `Transcribing→transcribing`, `Finalizing→finalizing`, `Error→error`; and the `Loading`-seen-but-not-`Ready` window → `loading`. Assert payloads carry **no transcript text** (C3). **Write first, observe fail, then satisfy with T008**
- [X] T008 Implement the `IndicatorState → State`-string mapping + the `loading`/`recording` session-tracking in `client/myna-desktop/src/indicator/dbus.rs` (pure function + the `Ready`-seen flag). Satisfies T007 (data-model E1; contract publisher.md P1/P2/P4)
- [X] T009 [P] GJS contract test `extensions/myna-shell/test/states.test.js` for `states.js`: every known `State` → its visual-intent record (colour class + animation + a11y label per data-model mapping); unknown `State` → neutral "active" intent (no throw); `idle` → hidden; `loading` ≠ `recording` (X1–X4). **Write first, observe fail, then satisfy with T010**
- [X] T010 [P] Implement pure `states.js` in `extensions/myna-shell/`: `stateToIntent(state) -> {cssClass, animation, a11yLabel}`, unknown-tolerant, content-free. Satisfies T009 (data-model mapping; contract extension.md X1–X4, X6)

**Checkpoint**: the contract is encoded and tested on both sides — the `Bus` seam + state mapping (Rust, red→green) and the pure `states.js` (GJS). User stories can now proceed.

---

## Phase 3: User Story 1 - See dictation state without losing focus (Priority: P1) 🎯 MVP

**Goal**: a focus-safe animated goop appears during a session and clears when it
ends, driven by the real `org.myna.Dictation` state, never stealing keyboard focus.

**Independent Test**: with the extension installed and `myna-desktop --dbus` running,
start a session while a text field is focused — the goop appears within the latency
target, typing still lands in the field, and the goop clears when the session ends.

### Tests (publisher: Rust TDD; extension: GJS harness-tier)

- [X] T011 [P] [US1] Hermetic test in `client/myna-desktop/tests/dbus_indicator.rs`: driving `DbusIndicator` through `Recording`→…→`Hidden` emits exactly one `StateChanged` per transition with the mapped `State` and updates the `State` property; `hide()` publishes `idle` and zeroes levels (C2, P3). **Write first, observe fail, then satisfy with T013**
- [X] T012 [P] [US1] GJS lifecycle test in `extensions/myna-shell/test/states.test.js` (or a sibling) against a **stub proxy**: `enable()` with the name absent stays dormant (no actor); name-appeared connects + reflects current `State`; name-vanished clears to idle; `disable()` tears down actors/timers/subscriptions (X7–X10). **Write first, observe fail, then satisfy with T015/T016**

### Implementation

- [X] T013 [US1] Implement `DbusIndicator` (impl `indicator::Indicator`) in `client/myna-desktop/src/indicator/dbus.rs`: `set_state`/`hide` emit `StateChanged` + update `State`/`ErrorMessage` via the `Bus` seam, using the T008 mapping. Satisfies T011 (contract publisher.md P1–P5)
- [X] T014 [US1] Wire a `--dbus` activation path into `client/myna-desktop/src/bin/myna-desktop.rs`: compose `DbusIndicator` as the controller's `Indicator` (with `NotifyIndicator` fallback when the bus is unavailable, P15), and stand the `org.myna.Dictation` object (skeleton from T024 if landed, else a temporary in-proc serve). Confirm `myna-desktop --dbus` starts and requests the name
- [X] T015 [P] [US1] Implement `dbus.js` in `extensions/myna-shell/`: a `Gio.DBusProxy` for `org.myna.Dictation` with `Gio.bus_watch_name` (appeared/vanished → available/dormant, R9), exposing `state`/`errorMessage` + a `StateChanged` callback. Satisfies part of T012 (contract extension.md X7/X8)
- [X] T016 [US1] Implement `extension.js` + the goop actor in `indicator.js`: `enable()` wires the proxy to an `St.DrawingArea`/`St.Widget` added to `Main.layoutManager` (Shell **chrome**, never a window → no focus steal), shown only when `state ≠ idle`, using the `states.js` intent + `stylesheet.css`; `disable()` destroys actors + cancels timers/subscriptions. Satisfies T012 (contract extension.md X8–X11; FR-001/002/003/009)
- [X] T017 [US1] Add the base goop geometry + appear/clear animation in `indicator.js`/`stylesheet.css` (R6): center-top hanging blob, ease-in on show, ease-out + destroy on `idle`, within the activation/teardown latency targets. (FR-003/004; SC-003)

**Checkpoint**: US1 is functional — a spoken session shows a focus-safe goop that appears/clears; the on-hardware focus-safety check (quickstart §5, X11/SC-001) can be run.

---

## Phase 4: User Story 2 - Read the current dictation state at a glance (Priority: P1)

**Goal**: the goop shows a visually distinct treatment for loading / recording /
transcribing / finalizing / error, so state is legible without any transcript text.

**Independent Test**: drive `myna-desktop` (or a stub publisher) through each state
and confirm the goop shows a distinct treatment for each, transitions promptly, and
degrades gracefully on an unknown state.

### Tests

- [ ] T018 [P] [US2] Hermetic test in `client/myna-desktop/src/indicator/dbus.rs` `#[cfg(test)]`: the `loading`/`recording` split — `Loading` before `Ready` publishes `loading`, then `recording` after `Ready`; `Error` carries the content-free reason via `ErrorMessage` + the `StateChanged` arg (C5, P2/P4). **Write first, observe fail, then satisfy with T020** (may already partly hold from T008 — extend for the reason arg)
- [ ] T019 [P] [US2] GJS contract test extending `states.test.js`: each state's intent is distinct (loading ≠ recording ≠ transcribing ≠ finalizing ≠ error), and the error intent surfaces the reason label; unknown → neutral (X4/X8, FR-005/006/007). **Write first, observe fail, then satisfy with T021/T022**

### Implementation

- [ ] T020 [US2] Finish the publisher error path in `client/myna-desktop/src/indicator/dbus.rs`: `Error(msg)` sets `ErrorMessage` + emits `StateChanged("error", msg)` (content-free reason from the existing controller messages). Satisfies T018 (data-model E3; contract publisher.md P4)
- [ ] T021 [US2] Implement the per-state visual treatments in `indicator.js`/`stylesheet.css` (R6): warm-amber breathing pulse (`loading`), ripple (`recording`), processing shimmer (`transcribing`), confirming flash (`finalizing`), red flash + shake then clear (`error`) — each a CSS class + Clutter animation keyed off the `states.js` intent. (FR-005/006/007/009; SC-002)
- [ ] T022 [US2] Handle unknown/extra states + transient error display in `indicator.js`: unknown → the neutral "active" treatment (no throw, FR-008); `error` shows briefly then returns to idle when the state clears (FR-007). Satisfies T019 (contract extension.md X13)

**Checkpoint**: US1+US2 = the MVP — a focus-safe, state-legible goop. quickstart §5 (X13/SC-002) is demonstrable.

---

## Phase 5: User Story 3 - See that my voice is being captured (Priority: P2)

**Goal**: a real-time VU/glow tied to captured level, that eases to floor on
silence/stale and shows nothing when idle.

**Independent Test**: with a session active, feed known levels through the interface
and confirm the glow tracks them (rises on loud, falls on silence), decays to floor
when updates lapse, and shows nothing when idle.

### Tests

- [ ] T023 [P] [US3] Hermetic test in `client/myna-desktop/src/dbus/mod.rs` `#[cfg(test)]`: fed a sequence of `AudioStats`, the level pump publishes `AudioRms`/`AudioPeak` from the latest stats while recording, throttled to ~15–20 Hz, and `0.0` at idle; never publishes samples/content (C4, P6/P7). **Write first, observe fail, then satisfy with T025**
- [ ] T024 [P] [US3] GJS contract test extending `states.test.js` for `vumeter.js`: level `[0,1]` → glow intensity is monotonic + clamped, and decays to floor when the last update is older than the stale window (~300 ms); no content in output (X5/X6, SC-004). **Write first, observe fail, then satisfy with T026/T027**

### Implementation

- [ ] T025 [US3] Implement the level pump in `client/myna-desktop/src/dbus/mod.rs`: a tokio task subscribing to `CaptureSource::stats()` (`watch::Receiver<AudioStats>`), publishing `AudioRms`/`AudioPeak` at ~15–20 Hz while a session is active and `0.0` at idle/end. Satisfies T023 (data-model E2; contract publisher.md P6–P8)
- [ ] T026 [US3] Implement pure `vumeter.js` in `extensions/myna-shell/`: `levelToGlow(level, ageMs) -> intensity` — monotonic, clamped, decaying to floor past the stale window. Satisfies T024 (contract extension.md X5)
- [ ] T027 [US3] Drive the goop glow/VU from the proxy's `AudioRms`/`AudioPeak` in `indicator.js` (R7): the glow radius/intensity is the VU, updated on property change, eased between updates and to floor on silence/stale via `vumeter.js`; nothing shown when idle. (FR-010/011/012; SC-004)

**Checkpoint**: US3 adds the live VU glow; quickstart §5 (X14/SC-004) demonstrable.

---

## Phase 6: User Story 4 - Start or stop dictation from the panel (Priority: P3)

**Goal**: an optional subtle panel button toggles a session equivalently to the
hotkey, dims when the daemon is absent, and preserves commit-only behavior.

**Independent Test**: click the panel button → a session starts (state leaves idle);
click again → it ends and commits, identical to the hotkey; button dims when
`org.myna.Dictation` is absent.

### Tests

- [ ] T028 [P] [US4] Hermetic test for `DbusTrigger` (impl `orchestrator::Trigger`) in `client/myna-desktop/src/shortcut/dbus.rs` `#[cfg(test)]`: `Toggle` alternates `Press`/`Release`; `Start`→`Press` when idle, `Stop`→`Release` when active; duplicate/rapid `Start`/`Toggle` do not start two sessions (dedup, mirrors `ControlTrigger`); `Start` returns `(false, reason)` when it cannot start (C6/C7, P9–P11). **Write first, observe fail, then satisfy with T030** (reuse `ControlTrigger`'s alternation test style)
- [ ] T029 [P] [US4] GJS test extending `states.test.js`: the panel button calls `Toggle` on click and reflects availability (dimmed when the name is absent) via the stub proxy (X16). **Write first, observe fail, then satisfy with T031**

### Implementation

- [ ] T030 [US4] Implement `DbusTrigger` in `client/myna-desktop/src/shortcut/dbus.rs`: `Start`/`Stop`/`Toggle` D-Bus methods feed `TriggerEdge`s into the orchestrator's `Trigger` seam with `ControlTrigger`-style alternation/dedup; `Start` returns `(ok, reason)`. Wire it into the `--dbus` mode of `bin/myna-desktop.rs` alongside the existing triggers. Satisfies T028 (data-model E1; contract publisher.md P9–P12, dbus-interface.md methods)
- [ ] T031 [US4] Implement the optional `PanelMenu.Button` in `indicator.js` (R8): a subtle symbolic glyph following GNOME HIG, dimmed when `org.myna.Dictation` has no owner, calling `Toggle()` on click; give non-intrusive feedback when the command is unavailable (FR-013/014/015). Satisfies T029 (contract extension.md X16)

**Checkpoint**: all four stories functional; quickstart §6 (X16/SC-010) demonstrable.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: the real `zbus` serve, gated integration, watermarks, a11y, docs, and
the end-to-end acceptance.

- [ ] T032 Implement the real `zbus`-backed `Bus` + serve the `org.myna.Dictation` object at `/org/myna/Dictation` in `client/myna-desktop/src/dbus/mod.rs`: request the well-known name, wire `State`/`AudioRms`/`AudioPeak`/`ErrorMessage` properties + `StateChanged` signal + `Start`/`Stop`/`Toggle` methods; release the name on shutdown (C1/C9, P13/P14). Replaces the temporary in-proc serve from T014
- [ ] T033 Env-gated integration suite in `client/myna-desktop/tests/dbus_hw.rs` (`MYNA_DBUS_TESTS=1`, via `dbus-run-session`): stand the object on a real session bus, assert a `zbus` client observes `StateChanged` + reads properties, and that name-appeared/vanished fire on start/shutdown (C1/C9; contract publisher.md P13–P15). Runs identically on VM + hardware (Principle II)
- [ ] T034 [P] Publisher watermark check in `client/myna-desktop/tests/watermarks.rs`: `StateChanged`→property-update latency and level-pump cadence within declared tolerances; assert no capture-path regression vs the feature-002/003 baselines (constitution III; contract publisher.md P8)
- [ ] T035 [P] Accessibility in `indicator.js`: set the goop's `accessible_name` to the human state label per state (Orca announces changes) and ensure a high-contrast CSS variant; legibility never relies on colour alone (shape/animation also differ) (FR-022; SC-009; contract extension.md X17)
- [ ] T036 [P] Version-gate verification: confirm `metadata.json` `shell-version: ["50","51"]` loads on the target Shell and the extension refuses to load elsewhere; document the install/enable flow (FR-020; SC-008; contract extension.md X18)
- [ ] T037 [P] Update `docs/desktop-injection.md` §2 to record this extension as the GNOME focus-safe overlay answer (NotifyIndicator remains the fallback), and add a short `extensions/myna-shell/README.md` (install, enable, the `org.myna.Dictation` contract, packaging-as-follow-up per R12). Note the possible constitution PATCH for a GJS-UI harness-tier carve-out (research Open items)
- [ ] T038 Run the quickstart end-to-end (§1–§8): hermetic + gated publisher green, GJS contract green, install/enable, the **on-hardware spoken run** (goop appears, **focus never stolen** while typing, states legible, VU tracks voice, transcript injected via IBus unchanged), panel toggle, robustness spot-checks (daemon crash → clears; disable → no leaks), watermarks recorded (SC-001–SC-010)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies — start immediately.
- **Foundational (Phase 2)**: depends on Setup — BLOCKS all user stories (encodes the shared contract on both sides).
- **User Stories (Phase 3–6)**: depend on Foundational. US1 and US2 are co-P1 (US2 builds on US1's actor); US3 (VU) and US4 (panel) are independent additions on top of the US1/US2 skeleton.
- **Polish (Phase 7)**: T032/T033 (real serve + gated suite) can begin once the publisher shape is stable (after Phase 3); T034–T038 depend on the relevant stories being present. T038 depends on all desired stories.

### User Story Dependencies

- **US1 (P1)**: after Foundational. Delivers the focus-safe goop skeleton + publisher `DbusIndicator`.
- **US2 (P1)**: after US1 (extends the same actor with per-state treatments + the publisher error/loading paths). Co-MVP with US1.
- **US3 (P2)**: after Foundational; integrates with the US1 actor for the glow but the level pump (T025) + `vumeter.js` (T026) are independent and can be built in parallel with US2.
- **US4 (P3)**: after Foundational; `DbusTrigger` (T030) is independent of the UI; the panel button (T031) integrates with the US1 actor.

### Within Each Story

- Tests before implementation (Rust publisher: red→green per Principle I; GJS: contract tests before the pure modules).
- Publisher mapping/seam before the actor that consumes the contract.
- Pure modules (`states.js`/`vumeter.js`) before the actor wiring that uses them.

### Parallel Opportunities

- Setup: T003 (gated-test file) ∥ T004 (GJS scaffold).
- Foundational: the Rust side (T005–T008) ∥ the GJS side (T009–T010) — different languages/files.
- Within a story, the `[P]` test tasks and the Rust-vs-GJS split run in parallel.
- Across stories after Foundational: US3's level pump/`vumeter.js` and US4's `DbusTrigger` can proceed alongside US2.

---

## Parallel Example: Foundational (Phase 2)

```bash
# Rust publisher contract (one developer):
Task T005/T006: Bus seam + FakeBus in client/myna-desktop/src/dbus/mod.rs
Task T007/T008: IndicatorState→State mapping in client/myna-desktop/src/indicator/dbus.rs

# GJS pure mapping (another developer, in parallel — different tree):
Task T009/T010: states.js + test/states.test.js in extensions/myna-shell/
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED — constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/tasks) | Prerequisite branches | Merge gates |
|---|--------|----------------------|-----------------------|-------------|
| 1 | `004a-shell-setup-foundation` | Phase 1–2 (T001–T010) | — | hermetic publisher suite green (fake bus); GJS `states.test.js` green; workspace builds both feature settings; Workshop def extended (Principle IV gate closes here) |
| 2 | `004b-focus-safe-goop-us1` | Phase 3 (T011–T017) | #1 | hermetic `dbus_indicator` green; GJS lifecycle test green; manual focus-safety check (SC-001) |
| 3 | `004c-state-treatments-us2` | Phase 4 (T018–T022) | #2 | hermetic loading/error split green; GJS distinct-intent test green; states legible (SC-002) |
| 4 | `004d-vu-glow-us3` | Phase 5 (T023–T027) | #2 (may land alongside #3) | hermetic level-pump green; GJS `vumeter` test green; VU tracks voice (SC-004) |
| 5 | `004e-panel-toggle-us4` | Phase 6 (T028–T031) | #2 | hermetic `DbusTrigger` dedup green; panel toggle equivalent to hotkey (SC-010) |
| 6 | `004f-serve-gated-polish` | Phase 7 (T032–T038) | #2 (US1 green); #3–#5 for docs completeness | full workspace + clippy green; env-gated `dbus_hw` green; watermarks recorded; quickstart §1–§8 pass incl. the on-hardware acceptance |

### MVP First (US1 + US2)

1. Phase 1: Setup → 2. Phase 2: Foundational (CRITICAL — the shared contract) →
3. Phase 3: US1 (focus-safe goop) → 4. Phase 4: US2 (state treatments) →
**STOP and VALIDATE**: focus is never stolen and every state is legible
(quickstart §5; SC-001/SC-002). This is the shippable MVP.

### Incremental Delivery

MVP (US1+US2) → add US3 (VU glow) → add US4 (panel toggle) → Polish (real serve,
gated suite, watermarks, a11y, docs, end-to-end acceptance). Each increment leaves
the default branch green (hermetic + the increment's gate).

---

## Notes

- [P] = different files, no dependency on incomplete tasks.
- The Rust publisher is TDD-first (Principle I); the GJS extension is harness-tier — pure logic is contract-tested, compositor behavior is the manual acceptance (T038).
- **Privacy invariant throughout**: only state + normalized level + a content-free error reason cross `org.myna.Dictation`; the goop renders/logs/persists no transcript; no audio is captured by either half; no network (constitution V).
- No new Rust crate and no new Rust dependency (reuse vendored `zbus` + the existing `AudioStats` watch + the `Indicator`/`Trigger` seams).
- Verify each test fails before implementing; commit after each task or logical group; stop at any checkpoint to validate a story independently.
