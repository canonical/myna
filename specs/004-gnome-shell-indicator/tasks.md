# Tasks: GNOME Shell Extension for Myna Dictation UI

**Input**: Design documents from `/specs/004-gnome-shell-indicator/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Regenerated 2026-07-30** for the HUD redesign. The original tasks.md (dated
2026-07-21) tracked the "goop" design; the codebase has since moved through an
intermediate `RibbonView` (2026-07-22, documented in research.md R6 but never
back-filled into that tasks.md) and is now moving to the bottom-center HUD pill.
This regeneration was preceded by an audit of the actual repository state (not
just the old checkboxes) so completed, still-valid infrastructure is marked
done and only the HUD-specific delta is left as new work. See "Audit notes"
under each phase for what was verified.

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
  on-hardware acceptance** (quickstart §5/5a/5b), not test-first.

**Organization**: Tasks grouped by user story (US1, US2, US2A, US3, US4) for
independent implementation and testing. Priority order: US1 (P1 🎯 MVP) + US2
(P1, co-MVP) + US2A (P1, co-MVP — severity legibility) → US3 (P2) → US4 (P3).
US2A is new (2026-07-30 clarify pass, "Tell a passing hiccup from a real
problem") and is co-P1 with US1/US2 per spec.md.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1/US2/US2A/US3/US4 for story-phase tasks; Setup/Foundational/Polish carry none
- All paths are repo-relative

## Path Conventions

- Rust publisher (shipped): `client/myna-desktop/src/{dbus/mod.rs,dbus/serve.rs,indicator/{mod,dbus,gtk,notify}.rs,shortcut/dbus.rs,bin/myna-desktop.rs,controller.rs}`
- Reused seams (unchanged): `client/myna-orchestrator/src/{trigger,sink,fsm}.rs`
- Reused levels (unchanged): `client/myna-audio` (`CaptureSource::stats()` → `watch::Receiver<AudioStats>`)
- Hermetic publisher tests: `client/myna-desktop/tests/{dbus_indicator.rs,controller.rs}` + `#[cfg(test)]` in the modules
- Env-gated publisher suite: `client/myna-desktop/tests/dbus_hw.rs` (gate: `MYNA_DBUS_TESTS=1`, via `dbus-run-session`)
- GJS extension bundle: `extensions/myna-shell/` (`metadata.json`, `extension.js`, `dbus.js`, `hud.js` [new], `hud-logic.js` [new — pure helpers factored out of `hud.js` so positioning/severity/replace-in-place decisions are headlessly testable], `states.js`, `view.js`, `vumeter.js`, `stylesheet.css`, `test/{states,hud,lifecycle,vumeter}.test.js`)
- Shared contract: `specs/004-gnome-shell-indicator/contracts/dbus-interface.md` (`org.myna.Dictation`)

---

## Phase 1: Setup (Shared Infrastructure)

**Audit notes**: verified already complete and unaffected by the HUD redesign —
Workshop deps, the Rust publisher module scaffolding, the env-gated test file
skeleton, and the GJS bundle scaffold (files exist: `extension.js`, `dbus.js`,
`states.js`, `view.js`, `vumeter.js`, `stylesheet.css`, `indicator.js` [the
`RibbonView`, slated for removal in US1 below], `metadata.json` with correct
`shell-version`).

- [X] T001 Workshop definition (constitution Principle IV) extended in `.workshop/myna.yaml` with session D-Bus + `gjs`/`gnome-shell` deps. *(Verified: no change needed for the HUD redesign.)*
- [X] T002 Rust publisher modules scaffolded in `client/myna-desktop/src/`: `dbus/mod.rs` (`Bus` seam, `DictationService`, `FakeBus`), `dbus/serve.rs` (real `zbus`-backed `ZbusBus`), `indicator/dbus.rs` (`DbusIndicator`), `shortcut/dbus.rs` (`DbusTrigger` stub only — see US4). *(Verified via `grep`/`read` against the live source.)*
- [X] T003 Env-gated test file `client/myna-desktop/tests/dbus_hw.rs` exists with a `MYNA_DBUS_TESTS` gate helper that skips cleanly when unset. *(Verified: the real round-trip assertions are still pending — see Polish T0XX below.)*
- [X] T004 GJS bundle scaffolded in `extensions/myna-shell/`: `metadata.json`, `extension.js`, `dbus.js`, `states.js`, `view.js`, `vumeter.js`, `stylesheet.css`, `indicator.js` (`RibbonView`), `test/{states,lifecycle,vumeter}.test.js`. *(Verified.)*
- [X] T005 [P] Update `metadata.json`'s `description` field to drop the "(the goop)" wording (2026-07-30: superseded by the HUD pill); no other field changes needed (`shell-version: ["50","51"]` already correct)

**Checkpoint**: workspace builds; the GJS bundle parses; existing suites green. Ready for the Foundational severity work.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the shared `org.myna.Dictation` contract, INCLUDING the new
`notice`/`error` severity split (R13), encoded and tested on both sides before
any story-specific HUD rendering begins. **⚠️ CRITICAL**: complete before
US2A (and ideally before US1/US2's HUD-rendering tasks, since `states.js`'s
reshaped descriptor is what `hud.js` consumes throughout).

**Audit notes**: the *original* Foundational contract (Bus seam + FakeBus,
`IndicatorState`→`State`-string mapping for the five pre-existing states,
pure `states.js` state→descriptor mapping) is verified complete and unaffected.
Only the severity-split work below is new.

- [X] T006 `Bus` seam trait + `FakeBus` in `client/myna-desktop/src/dbus/mod.rs`, recording property sets. *(Verified.)*
- [X] T007 `IndicatorState → State`-string mapping for `idle`/`loading`/`recording`/`transcribing`/`finalizing`/`error` in `client/myna-desktop/src/indicator/dbus.rs` (`map_state`, incl. the R4 `loading`/`recording` split). *(Verified: `map_state` and its tests exist exactly as data-model E1 describes for these five states.)*
- [X] T008 Pure `states.js` state→descriptor mapping in `extensions/myna-shell/states.js` (`stateToDescriptor`), unknown-tolerant, content-free. *(Verified — but see T012 below: the descriptor shape needs reshaping for severity.)*

### Severity split (2026-07-30, R13) — NEW

- [X] T009 [P] Hermetic test in `client/myna-desktop/src/indicator/mod.rs` / `indicator/dbus.rs` `#[cfg(test)]`: `IndicatorState::Error{message, recoverable}` — construct with `recoverable: true` and `recoverable: false`, assert `map_state` returns the wire `notice` string for the former and `error` for the latter (C10). **Write first, observe fail (does not compile — `Error` is still a tuple variant), then satisfy with T010**
- [X] T010 Change `IndicatorState::Error(String)` to `IndicatorState::Error { message: String, recoverable: bool }` in `client/myna-desktop/src/indicator/mod.rs`; update `map_state` in `indicator/dbus.rs` to emit `wire_state::NOTICE` (`"notice"`, new const) when `recoverable == true` and `wire_state::ERROR` otherwise. Satisfies T009 (data-model E1/E1a; contract publisher.md P16)
- [X] T011 [P] Mechanical updates (behavior UNCHANGED, each with a test asserting so) in `client/myna-desktop/src/indicator/gtk.rs` and `client/myna-desktop/src/indicator/notify.rs`: destructure `Error{message, ..}` in place of `Error(message)`/`Error(_)`; assert every existing error-rendering test still passes byte-for-byte (constitution I — even "unchanged" behavior gets a red-green pair here since the match arms had to be touched). Satisfies contract publisher.md P19
- [X] T012 [P] Update `client/myna-desktop/tests/dbus_indicator.rs` and `client/myna-desktop/tests/controller.rs` construction sites (`IndicatorState::Error("...".into())` → `IndicatorState::Error{message: "...".into(), recoverable: false}`) so the existing suites compile and stay green
- [X] T013 [P] Hermetic test in `client/myna-desktop/src/controller.rs` `#[cfg(test)]`: `completion_indicator_state("")`/`completion_indicator_state("   ")` → `IndicatorState::Error{message: "No speech detected", recoverable: true}`; `completion_indicator_state("hello")` → `IndicatorState::Hidden`. **Write first, observe fail, then satisfy with T014**
- [X] T014 Implement `fn completion_indicator_state(transcript: &str) -> IndicatorState` in `client/myna-desktop/src/controller.rs`; wire it into **both** the live `event_to_indicator`'s `OrchestratorEvent::Done(text)` arm and the finalize-block `Ok(SessionOutcome::Completed{transcript})` handler (replacing their current hardcoded `Hidden`). Satisfies T013 (data-model E1a; contract dbus-interface.md C10/C11, publisher.md P17)
- [X] T015 Hermetic test in `client/myna-desktop/tests/controller.rs`: assert the live-event path and the finalize-block path produce **identical** `IndicatorState` for the same transcript (never disagree), and that a redundant second `DbusIndicator::publish` call for the same wire state is a no-op (C11, P18). **Write first, observe fail, then satisfy by wiring both call sites through the same T014 helper**
- [X] T016 [P] GJS contract test extending `extensions/myna-shell/test/states.test.js`: `stateToDescriptor('notice', reason)` returns `{key: 'notice', statusText: reason, severity: 'recoverable', hidden: false}`; `stateToDescriptor('error', reason)` returns `{..., severity: 'critical'}`; all five pre-existing states return `severity: null`; unknown states still degrade to neutral with `severity: null`. **Write first, observe fail, then satisfy with T017**
- [X] T017 Reshape the descriptor in `extensions/myna-shell/states.js`: replace `{key, statusText, isError, hidden}` with `{key, statusText, severity, hidden}` where `severity` is `'recoverable' | 'critical' | null`; add the `notice` entry to `DESCRIPTORS`. Satisfies T016 (data-model E1 mapping table; contract extension.md X19)

**Checkpoint**: the full contract — including severity — is encoded and tested on both sides. HUD-rendering story work (US1/US2/US2A/US3) can now proceed.

---

## Phase 3: User Story 1 - See dictation state without losing focus (Priority: P1) 🎯 MVP

**Goal**: a focus-safe HUD pill appears bottom-center of the screen during a
session and clears when it ends, driven by the real `org.myna.Dictation`
state, never stealing keyboard focus.

**Independent Test**: with the extension installed and `myna-desktop --dbus`
running, start a session while a text field is focused — the pill appears
within the latency target at the bottom-center of the screen, typing still
lands in the field, and the pill clears when the session ends.

**Audit notes**: `DbusIndicator`, the `--dbus` activation path, and `dbus.js`'s
proxy/lifecycle are all verified complete and unaffected by the HUD redesign
(they serve the *contract*, not the *view*). Only the presentation
(`indicator.js`'s `RibbonView`, positioned top-of-panel) needs replacing.

- [X] T018 `DbusIndicator` (impl `indicator::Indicator`) in `client/myna-desktop/src/indicator/dbus.rs`. *(Verified — will need the T010 field-shape update from Foundational, already covered there.)*
- [X] T019 `--dbus` activation path in `client/myna-desktop/src/bin/myna-desktop.rs`, with `NotifyIndicator` fallback. *(Verified.)*
- [X] T020 `dbus.js` proxy + `Gio.bus_watch_name` lifecycle (dormant/appeared/vanished) in `extensions/myna-shell/dbus.js`. *(Verified — unchanged by this redesign per plan.md.)*

### Tests (GJS harness-tier)

- [X] T021 [P] [US1] GJS contract test `extensions/myna-shell/test/hud.test.js` (new file): the HUD pill actor is added via `Main.layoutManager.addChrome` (not `addTopChrome`) and positioned bottom-center of the primary monitor — `y == monitor.y + monitor.height - HEIGHT - MARGIN`, horizontally centered; repositions on a stubbed `monitors-changed` (X21, FR-004). **Write first, observe fail, then satisfy with T023**

### Implementation

- [X] T022 [US1] Implement the base HUD pill actor in a new `extensions/myna-shell/hud.js`: an `St.Widget`-based `HudView` implementing the existing `IndicatorView` seam (`show`/`setLevel`/`hide`/`destroy` per `view.js`), non-reactive/non-focusable chrome (X11/SC-001), added via `Main.layoutManager.addChrome`. Wire `view.js`'s `createView()` factory to return `HudView` by default
- [X] T023 [US1] Bottom-center positioning in `hud.js` (R14): compute position from `Main.layoutManager.primaryMonitor` at `monitor.y + monitor.height - HEIGHT - MARGIN`, centered horizontally; re-position on `monitors-changed`. Satisfies T021 (contract extension.md X21)
- [X] T024 [US1] Appear/clear animation for the pill in `hud.js`/`stylesheet.css`: ease-in on `show()`, ease-out + destroy on `hide()`, within the activation/teardown latency targets (FR-003; SC-003)
- [X] T025 [US1] Delete `extensions/myna-shell/indicator.js` (the `RibbonView`) and its now-superseded test coverage, per spec Assumptions ("goop removed, not retained")

**Checkpoint**: US1 is functional — a spoken session shows a focus-safe, bottom-center HUD pill that appears/clears; the on-hardware focus-safety check (quickstart §5, X11/SC-001) can be run.

---

## Phase 4: User Story 2 - Read the current dictation state at a glance (Priority: P1)

**Goal**: the HUD pill shows a visually distinct treatment (icon + label) for
loading / recording / transcribing / finalizing, so state is legible without
any transcript text. (The `error`/`notice` severity treatments are US2A,
below — they share this story's priority but are split out because they were
a later clarify-pass addition with their own independent test.)

**Independent Test**: drive `myna-desktop` (or a stub publisher) through
loading/recording/transcribing/finalizing and confirm the HUD pill shows a
distinct treatment for each, transitions promptly, and degrades gracefully on
an unknown state.

**Audit notes**: the publisher-side mapping for these four states is verified
complete (T007 above). Only the HUD-specific rendering is new.

### Tests

- [X] T026 [P] [US2] GJS contract test extending `extensions/myna-shell/test/hud.test.js`: each of loading/recording/transcribing/finalizing renders a distinct icon+label combination via `HudView.show()`; an unknown state falls back to the neutral "active" treatment without throwing (X13, FR-005/006/008). **Write first, observe fail, then satisfy with T027/T028**

### Implementation

- [X] T027 [US2] Implement the per-state HUD treatment in `hud.js`/`stylesheet.css`: a filled mic icon + the `states.js` `statusText` label for loading/recording/transcribing/finalizing, each visually distinct (icon state/colour/label combination — no animation-family requirement carried over from the goop/ribbon eras). Satisfies part of T026 (FR-005/006/009; SC-002)
- [X] T028 [US2] Handle the unknown-state neutral fallback in `hud.js`: an unrecognized `State` renders the neutral "active" treatment (mic icon, generic label) without throwing (FR-008). Satisfies T026 (contract extension.md X2)

**Checkpoint**: US1+US2 = the MVP skeleton — a focus-safe, state-legible HUD pill for the non-error lifecycle. quickstart §5 (X13/SC-002) is demonstrable for these four states.

---

## Phase 5: User Story 2A - Tell a passing hiccup from a real problem (Priority: P1)

**Goal**: a recoverable issue (e.g. "no speech detected") shows as a
non-blocking, auto-dismissing notice that never blocks a new session; a
critical error (e.g. microphone unavailable) shows as a persistent notice
with an explicit dismiss (×) control that is clickable but never
keyboard-focusable.

**Independent Test**: drive `myna-desktop` through (a) a session that
finalizes with an empty transcript and (b) a simulated hard failure; assert
(a) auto-clears within the hold window and never blocks a new session, and
(b) persists until the × is clicked, with focus never leaving the user's
focused application when clicking it.

**Depends on**: Foundational (severity split, T009–T017) and US1 (the base
HUD pill actor, T022).

### Tests

- [X] T029 [P] [US2A] GJS contract test extending `test/hud.test.js`: `HudView.show({severity: 'recoverable', ...})` renders the mic icon (not slashed) and starts an auto-dismiss timer; `HudView.show({severity: 'critical', ...})` renders the mic-with-slash icon and a reactive-but-non-focusable dismiss control, with no timer (X19, X22, FR-007a/b). **Write first, observe fail, then satisfy with T031–T033**. *(Implemented as pure-logic tests against a new `hud-logic.js` module — `iconForSeverity`/`severityAutoDismisses` — factored out of `hud.js` precisely so this is headlessly testable, since `HudView` itself imports Shell/Clutter and cannot run outside a GNOME session, same constraint the prior `RibbonView` had. The dismiss control's actual click/focus behavior is manual-acceptance-only, quickstart §5b.)*
- [X] T030 [P] [US2A] GJS contract test: a second `show()` call with the *same* severity while one is already showing replaces the reason in place; for `recoverable` it restarts the auto-dismiss timer in full; for `critical` it does not waive the dismiss requirement (X20, FR-007a/FR-007d, R15). **Write first, observe fail, then satisfy with T034**. *(Implemented as pure-logic tests against `hud-logic.js`'s `shouldReplaceHeldNotice`/`severityAutoDismisses` — the actual timer-restart mechanics in `HudView._applyDescriptor` are Shell/GLib-dependent and covered by manual acceptance, quickstart §5a/5b.)*

### Implementation

- [X] T031 [US2A] Implement the "held notice" slot in `hud.js`: one severity-scoped slot (reason string + optional dismiss-timer handle) driving the recoverable/critical rendering split, per data-model E4
- [X] T032 [US2A] Implement the recoverable-notice auto-dismiss timer in `hud.js` (~3.5 s hold, matching the prior `ERROR_HOLD_MS` constant), calling the view's own teardown — never blocking a new session from starting while it's visible (FR-007a). Satisfies part of T029
- [X] T033 [US2A] Implement the critical-error dismiss (×) control in `hud.js`: a small reactive (`reactive: true`), never-focusable (`can_focus: false`) actor that clears the held notice on click, with no auto-dismiss (FR-007b/FR-007c). Satisfies part of T029 (contract extension.md X22)
- [X] T034 [US2A] Implement the replace-in-place + restart-timer logic in `hud.js`'s held-notice slot (R15): a second arrival of the same severity updates the reason and, for `recoverable`, restarts the timer in full. Satisfies T030 (contract extension.md X20)
- [X] T035 [US2A] Contextual mic icon in `hud.js`/`stylesheet.css`: filled mic for all non-critical treatments (including `recoverable`); mic-with-slash only for `critical` (data-model state→visual-intent table; contract extension.md X19)

**Checkpoint**: US1+US2+US2A complete the MVP — quickstart §5a/5b (recoverable and critical walkthroughs) are demonstrable, including the focus-safety check on the dismiss control (X11).

---

## Phase 6: User Story 3 - See that my voice is being captured (Priority: P2)

**Goal**: a segmented bar meter tied to captured level, that eases to floor
on silence/stale and shows nothing when idle.

**Independent Test**: with a session active, feed known levels through the
interface and confirm the bar meter tracks them (rises on loud, falls on
silence), decays to floor when updates lapse, and shows nothing when idle.

**Audit notes**: the publisher-side level pump (`AudioRms`/`AudioPeak` at
~15–20 Hz, zero at idle) and `vumeter.js`'s `levelToIntensity`/`FLOOR`
stale-decay logic are both verified complete and reused unchanged (R16) —
only the *rendering* (bars instead of the ribbon-era glow) is new.

- [X] T036 Level pump publishing `AudioRms`/`AudioPeak` in `client/myna-desktop/src/dbus/mod.rs` / `bin/myna-desktop.rs`. *(Verified.)*
- [X] T037 Pure `vumeter.js` (`levelToIntensity`, `FLOOR`, stale-decay) in `extensions/myna-shell/vumeter.js`. *(Verified — reused unchanged, R16.)*

### Tests

- [X] T038 [P] [US3] GJS contract test extending `test/hud.test.js`: `HudView.setLevel(rms, peak)` drives a fixed-count segmented bar meter (heights monotonic in level, clamped, decaying to floor past the stale window); no level rendered when hidden (X14, SC-004). **Write first, observe fail, then satisfy with T039**. *(Discovered `vumeter.js` already had `levelToBars` built and tested — `test/vumeter.test.js`'s existing "bar profile" assertions already cover monotonicity/clamping/stale-decay for the bar shape (pre-dating this task). `hud.js`'s `BarMeterActor` is a thin Cairo renderer over it; the actual on-screen rendering is manual-acceptance-only.)*

### Implementation

- [X] T039 [US3] Implement the segmented bar meter in `hud.js`/`stylesheet.css` (R16): a fixed set of discrete bars (e.g. 5–7) whose heights are driven by `vumeter.js`'s existing `levelToIntensity`, replacing the ribbon-era glow entirely. Satisfies T038 (contract extension.md X14). *(Implemented as `BarMeterActor` — 24 bars via `vumeter.js`'s existing `levelToBars`, which was already built and tested; `hud.js` only had to add the Cairo rendering + a repaint timer.)*
- [X] T039a [US3] **(2026-07-30, post-manual-test follow-up, R16a)** Two real bugs surfaced only in a live GNOME session, neither catchable by the headless suite: (1) `dbus.js` deduplicated numerically-identical level updates, so a steady voice stopped refreshing the stale-decay timestamp and the meter went flat after ~300 ms — fixed by forwarding every level update regardless of repeated values. (2) The original exponential gain curve needed shouting to move — recalibrated against a live Blackwire C5220 capture (`DB_FLOOR=-67`, `DB_CEILING=-14`, RMS+weighted-peak blend) so normal speech lands mid-meter. Replaced the symmetric bar-height profile with a conventional left-to-right segment count (`intensityToActiveSegments`) colour-zoned green/yellow/red (`segmentColor`); removed the now-dead `levelToIntensity`/`levelToBars` functions. New tests: `test/lifecycle.test.js` (repeated-level regression), `test/vumeter.test.js` (hardware-calibration + colour-zone assertions).

**Checkpoint**: US3 adds the live bar meter; quickstart §5 (X14/SC-004) demonstrable.

---

## Phase 7: User Story 4 - Start or stop dictation from the panel (Priority: P3)

**Goal**: an optional subtle panel button toggles a session equivalently to
the hotkey, dims when the daemon is absent, and preserves commit-only
behavior.

**Independent Test**: click the panel button → a session starts (state
leaves idle); click again → it ends and commits, identical to the hotkey;
button dims when `org.myna.Dictation` is absent.

**Audit notes**: **not built** — `shortcut/dbus.rs` is still a 9-line stub
(`pub struct DbusTrigger;`) and no `PanelMenu.Button` exists in the JS bundle.
This story is genuinely unaffected by the HUD redesign (the panel button is
independent of the pill) and remains exactly as originally scoped.

### Tests

- [ ] T040 [P] [US4] Hermetic test for `DbusTrigger` (impl `orchestrator::Trigger`) in `client/myna-desktop/src/shortcut/dbus.rs` `#[cfg(test)]`: `Toggle` alternates `Press`/`Release`; `Start`→`Press` when idle, `Stop`→`Release` when active; duplicate/rapid `Start`/`Toggle` do not start two sessions (dedup, mirrors `ControlTrigger`); `Start` returns `(false, reason)` when it cannot start (C6/C7, P9–P11). **Write first, observe fail, then satisfy with T042**
- [ ] T041 [P] [US4] GJS test extending `extensions/myna-shell/test/lifecycle.test.js`: the panel button calls `Toggle` on click and reflects availability (dimmed when the name is absent) via the stub proxy (X16). **Write first, observe fail, then satisfy with T043**

### Implementation

- [ ] T042 [US4] Implement `DbusTrigger` in `client/myna-desktop/src/shortcut/dbus.rs`: `Start`/`Stop`/`Toggle` D-Bus methods feed `TriggerEdge`s into the orchestrator's `Trigger` seam with `ControlTrigger`-style alternation/dedup; `Start` returns `(ok, reason)`. Wire it into the `--dbus` mode of `bin/myna-desktop.rs` alongside `DbusIndicator`. Satisfies T040 (data-model E1; contract publisher.md P9–P12, dbus-interface.md methods)
- [ ] T043 [US4] Implement the optional `PanelMenu.Button` in `extension.js` (R8): a subtle symbolic glyph following GNOME HIG, dimmed when `org.myna.Dictation` has no owner, calling `Toggle()` on click; give non-intrusive feedback when the command is unavailable (FR-013/014/015). Satisfies T041 (contract extension.md X16)

**Checkpoint**: all stories functional; quickstart §6 (X16/SC-010) demonstrable.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: the env-gated integration assertions, watermarks, legibility,
docs, and the end-to-end acceptance for the HUD redesign.

**Audit notes**: the real `zbus`-backed `Bus` (`ZbusBus` in
`client/myna-desktop/src/dbus/serve.rs`) is verified complete. The env-gated
round-trip *assertions* are not — `dbus_hw.rs` only contains the skip-gate
today.

- [X] T044 Real `zbus`-backed `Bus` serving `org.myna.Dictation` at `/org/myna/Dictation` in `client/myna-desktop/src/dbus/serve.rs`: `State`/`AudioRms`/`AudioPeak`/`ErrorMessage` properties via `PropertiesChanged`, name request/release (C1/C9, P13/P14). *(Verified.)*
- [ ] T045 Env-gated round-trip assertions in `client/myna-desktop/tests/dbus_hw.rs` (`MYNA_DBUS_TESTS=1`, via `dbus-run-session`): stand the object on a real session bus, assert a `zbus` client observes `PropertiesChanged` + reads properties including the new `notice` state, and that name-appeared/vanished fire on start/shutdown (C1/C9/C10; contract publisher.md P13–P15). Runs identically on VM + hardware (Principle II)
- [ ] T046 [P] Publisher watermark check in `client/myna-desktop/tests/watermarks.rs`: state-push→property-update latency (including the `notice`/`error` severity split) and level-pump cadence within declared tolerances; assert no capture-path regression (constitution III; contract publisher.md P8)
- [ ] T047 [P] High-contrast legibility in `hud.js`/`stylesheet.css`: a high-contrast CSS variant for the pill; legibility never relies on colour alone (icon/label also differ per severity). *(Screen-reader/AT-SPI announcement is explicitly out of scope here — tracked separately as T56 per spec FR-022.)*
- [ ] T048 [P] Version-gate verification: confirm `metadata.json` `shell-version: ["50","51"]` loads on the target Shell and the extension refuses to load elsewhere (FR-020; SC-008; contract extension.md X18)
- [ ] T049 [P] Update `docs/desktop-injection.md` §2 to record this extension as the **shipped** GNOME focus-safe overlay answer (not just option (a) among alternatives) — `NotifyIndicator` remains the fallback; add a short `extensions/myna-shell/README.md` (install, enable, the `org.myna.Dictation` contract including the `notice` state, packaging-as-follow-up per R12)
- [ ] T050 Run the quickstart end-to-end (§1–§8 incl. new §5a/§5b): hermetic + gated publisher green, GJS contract green (`states.test.js`, `hud.test.js`, `lifecycle.test.js`), install/enable, the **on-hardware spoken run** (HUD pill appears bottom-center, **focus never stolen** while typing or dismissing a critical error, states legible, bar meter tracks voice, transcript injected via IBus unchanged), the recoverable/critical severity walkthroughs, panel toggle, robustness spot-checks (daemon crash → clears; disable → no leaks), watermarks recorded (SC-001–SC-010)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: T005 has no dependencies — can run immediately (everything else already verified done).
- **Foundational (Phase 2)**: T009–T017 (severity split) — BLOCKS US2A entirely, and blocks any HUD rendering task that reads `severity` off the descriptor (US1's base actor does not need it, but US2/US2A do).
- **User Stories (Phase 3–7)**: US1 (T021–T025) can start once Foundational's T017 lands (states.js reshape) since `hud.js` is written against the new descriptor shape from the start. US2 (T026–T028) and US2A (T029–T035) both build on US1's base actor (T022). US3 (T038–T039) and US4 (T040–T043) are independent additions on top of the US1 actor.
- **Polish (Phase 8)**: T045/T046 depend on the publisher shape being stable (after Foundational); T047–T050 depend on the relevant stories being present. T050 depends on all desired stories.

### User Story Dependencies

- **US1 (P1)**: after Foundational's states.js reshape (T017). Delivers the focus-safe, bottom-center HUD pill skeleton.
- **US2 (P1)**: after US1 (extends the same actor with per-state treatments). Co-MVP with US1.
- **US2A (P1)**: after Foundational's severity split (T009–T017) AND US1 (T022). Co-MVP with US1/US2.
- **US3 (P2)**: after US1; the level pump and `vumeter.js` are already done, so this phase is HUD-rendering-only.
- **US4 (P3)**: after Foundational; `DbusTrigger` (T042) is independent of the UI; the panel button (T043) is independent of the HUD actor entirely (lives in `extension.js`, not `hud.js`).

### Within Each Story

- Tests before implementation (Rust publisher: red→green per Principle I; GJS: contract tests before the pure modules).
- The severity split (Foundational) before any story-level rendering of `notice`/`error`.
- `hud.js`'s base actor (US1) before any story adds a rendering concern to it (US2/US2A/US3).

### Parallel Opportunities

- Foundational: T009–T012 (Rust severity field + mechanical ripple) ∥ T016–T017 (GJS descriptor reshape) — different languages/files.
- US2A: T029 ∥ T030 (different test concerns, same file — sequential within the file but independent in intent).
- Across stories after US1: US3's bar meter (T038–T039) and US4's `DbusTrigger`/panel button (T040–T043) can proceed alongside US2A.

---

## Parallel Example: Foundational severity split (Phase 2)

```bash
# Rust severity field (one developer):
Task T009/T010: IndicatorState::Error{recoverable} + map_state split in indicator/{mod,dbus}.rs
Task T011/T012: mechanical gtk.rs/notify.rs/test updates
Task T013/T014/T015: completion_indicator_state + dual-call-site agreement in controller.rs

# GJS descriptor reshape (another developer, in parallel — different tree):
Task T016/T017: severity-shaped descriptor in states.js + test/states.test.js
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED — constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/tasks) | Prerequisite branches | Merge gates |
|---|--------|----------------------|-----------------------|-------------|
| 1 | `004g-severity-foundation` | Phase 1–2 (T005, T009–T017) | — | hermetic Rust severity tests green (fake bus); GJS `states.test.js` green with the reshaped descriptor; workspace + existing gtk/notify tests still green |
| 2 | `004h-hud-pill-us1` | Phase 3 (T021–T025) | #1 | GJS `hud.test.js` positioning test green; manual focus-safety check (SC-001) on the new bottom-center pill; `indicator.js` removed |
| 3 | `004i-state-treatments-us2` | Phase 4 (T026–T028) | #2 | GJS distinct-treatment test green; states legible (SC-002) for the four non-severity states |
| 4 | `004j-severity-us2a` | Phase 5 (T029–T035) | #2, #1 | GJS severity/replace-in-place/dismiss tests green; manual walkthrough of quickstart §5a/§5b incl. focus-safety on the × control (SC-001) |
| 5 | `004k-bar-meter-us3` | Phase 6 (T038–T039) | #2 (may land alongside #3/#4) | GJS bar-meter test green; meter tracks voice (SC-004) |
| 6 | `004l-panel-toggle-us4` | Phase 7 (T040–T043) | #1 | hermetic `DbusTrigger` dedup green; panel toggle equivalent to hotkey (SC-010) |
| 7 | `004m-gated-polish` | Phase 8 (T045–T050) | #2 (US1 green); #3–#6 for docs completeness | full workspace + clippy green; env-gated `dbus_hw` green incl. `notice` state; watermarks recorded; quickstart §1–§8 incl. §5a/§5b pass on hardware |

### MVP First (US1 + US2 + US2A)

1. Phase 1: Setup (T005) → 2. Phase 2: Foundational (CRITICAL — the severity
contract) → 3. Phase 3: US1 (bottom-center HUD pill) → 4. Phase 4: US2 (state
treatments) → 5. Phase 5: US2A (severity) →
**STOP and VALIDATE**: focus is never stolen (including when dismissing a
critical error), every state/severity is legible, and the recoverable notice
never blocks a new session (quickstart §5/§5a/§5b; SC-001/SC-002/SC-009). This
is the shippable MVP.

### Incremental Delivery

MVP (US1+US2+US2A) → add US3 (bar meter) → add US4 (panel toggle) → Polish
(gated suite assertions, watermarks, legibility, docs, end-to-end acceptance).
Each increment leaves the default branch green (hermetic + the increment's
gate).

---

## Notes

- [P] = different files, no dependency on incomplete tasks.
- The Rust publisher is TDD-first (Principle I); the GJS extension is harness-tier — pure logic is contract-tested, compositor behavior is the manual acceptance (T050).
- **Privacy invariant throughout**: only state (incl. the `notice`/`error` severity split) + normalized level + a content-free reason cross `org.myna.Dictation`; the HUD pill renders/logs/persists no transcript; no audio is captured by either half; no network (constitution V). The empty-transcript check that produces `notice` happens server-side — only its boolean outcome crosses the bus.
- No new Rust crate and no new Rust dependency (reuse vendored `zbus` + the existing `AudioStats` watch + the `Indicator`/`Trigger` seams).
- The `IndicatorState::Error` field addition (T010) is a mechanical, compiler-forced ripple across exactly 6 files (T010–T012) — see plan.md Complexity Tracking for why this shape was chosen over the alternatives.
- Verify each test fails before implementing; commit after each task or logical group; stop at any checkpoint to validate a story independently.
