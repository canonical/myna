# Implementation Plan: Switchable Basic Dictation HUD

**Branch**: `009-switchable-basic-hud` | **Date**: 2026-07-31 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/009-switchable-basic-hud/spec.md`

## Summary

Add a basic GNOME-OSD-style dictation HUD as the default while preserving the
existing wave-ribbon HUD as a selectable alternative. A persistent extension
preference (`basic` or `wave`) changes the active presentation live. A new
presentation controller owns state that must survive view replacement (current
descriptor, held notice/deadline, dismissal state, and timestamped level), while
both views remain rendering-only implementations of the `IndicatorView` seam.
The existing `org.myna.Dictation` publisher and wire contract do not change.

## Technical Context

**Language/Version**: GJS JavaScript (ES modules / SpiderMonkey) targeting GNOME Shell 50 and 51; XML for the GSettings schema; CSS for Shell theme rules.

**Primary Dependencies**: GNOME Shell platform modules (`St`, `Clutter`, `Atk`, `Gio`, `GLib`, `Main`); GTK4 + libadwaita in the separate preferences process; existing `org.myna.Dictation` D-Bus consumer, `states.js`, `vumeter.js`, and wave-ribbon modules. No third-party packages.

**Storage**: One per-user GSettings enum preference, `hud-style = basic | wave`, default `basic`. All dictation/level/notice state remains transient in memory; no audio or transcript storage.

**Testing**: Headless GJS contract tests through the existing Workshop `gjs-test` action; fake clock/scheduler/view tests for switching and notice lifetime; pure view-selection and basic-meter tests; `gnome-extensions pack` schema/package smoke check; manual GNOME Shell acceptance for compositor rendering and focus safety.

**Target Platform**: Ubuntu Desktop 26.10+ / GNOME Shell 50 and 51 on Wayland, consistent with feature 004 metadata. A real GNOME session is required only for actor/focus acceptance.

**Project Type**: Desktop Shell extension plus its standard preferences process; no Rust, server, D-Bus publisher, or snap changes.

**Performance Goals**: Apply a preference change and show the replacement within 250 ms; preserve the established state-to-visual update target (<50 ms); basic meter refresh around 20–30 Hz and decay to empty within 600 ms; sustain approximately 60 fps compositor rendering on reference hardware; after 100 switches exactly one view remains responsive.

**Constraints**: Exactly one view active; primary-monitor placement; no Shell restart for preference changes; no session interruption; no private GNOME volume OSD API; no focus stealing; no raw audio/transcript content; offline; preserve notice remaining lifetime and level arrival timestamp across replacement; destroy every retired actor, timer, transition, signal, and callback.

**Scale/Scope**: One extension bundle; two view implementations; one enum preference and one-row preferences page; one presentation controller; reuse the existing state mapping, calibrated meter input, D-Bus service, wave renderer, CSS shell, and Workshop environment. No publisher protocol change.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-checked after Phase 1 design.*

This feature extends the GJS GNOME Shell shim introduced by feature 004. GJS is
platform-mandated for in-compositor UI and is treated as evaluation-harness-tier
under the repo's recorded feature-004 exception: exempt from the Rust-language,
strict red-green, and checked-in performance-watermark requirements. The feature
still adds behavior-first headless contract tests and manual compositor
acceptance. Privacy/offline remains binding in full.

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD | Pure preference normalization, controller state transitions, switching, deadline preservation, stale-level handling, and meter mapping receive headless tests before implementation. Shell actor construction remains manual-acceptance-only because `St`/Clutter actors cannot run outside a compositor. | PASS by design; actor layer uses existing harness-tier exemption |
| II. Integration-Test Readiness | D-Bus inputs remain behind `DictationService`; controller tests inject fake views/clock/scheduler/settings. The same installed bundle is exercised on VM or hardware without source changes. | PASS |
| III. Performance Watermarks | The spec defines 250 ms switch, 600 ms decay, and 100-switch leak targets. Pure timings are deterministic tests; compositor smoothness and actor cleanup are manual observations under the existing harness-tier exemption. Publisher/capture watermarks are unaffected. | PASS; extension watermark exemption retained |
| IV. Workshop-Based Dev Environment | Existing desktop SDK already supplies GJS, GTK4/libadwaita, GNOME Shell, and GLib schema tools. The existing `gjs-test` action is reused and added to CI; package smoke validation uses the same environment. No host-only dependency is introduced. | PASS |
| V. Privacy-First, Offline-First | Preference stores only `basic`/`wave`; controller and views receive content-free state/reason and normalized level only. No samples, transcript, network, or new diagnostics. | PASS |
| Staged delivery | Tasks must stage controller/seam, basic view, preferences/package, then integrated parity/acceptance, each with its applicable tests and a green default branch. | PASS by plan |

**Gate result before Phase 0**: PASS. No unjustified violation.

## Phase 0: Research Decisions

Research is consolidated in [research.md](./research.md). The important resolved
decisions are:

1. Use an extension-local GSettings enum schema and standard libadwaita
   preferences page; listen to `changed::hud-style` for live updates.
2. Put presentation-independent lifecycle state in a controller above both
   views. Replaying a descriptor during a style switch is not a new state event.
3. Preserve an absolute recoverable-notice deadline and each level's original
   monotonic arrival timestamp across a switch.
4. Keep the existing `HudView`/`hud.js` as the wave implementation to avoid a
   noisy rename; add `BasicHudView` separately.
5. Reuse calibrated `levelsToIntensity()` but normalize its nonzero wave floor
   to a zero-based basic-bar fill; animate only while recording and decay to
   empty otherwise.
6. Compile the local schema during manual installation and package-smoke it in
   CI; do not commit `gschemas.compiled`.

No `NEEDS CLARIFICATION` items remain.

## Phase 1: Design

### Ownership and flow

```text
org.myna.Dictation ──> DictationService ──> stateToDescriptor
                                                │
GSettings hud-style ────────────────────────────┤
                                                v
                                  IndicatorController
                                  - current wire descriptor
                                  - displayed/held descriptor
                                  - recoverable deadline
                                  - dismissal state
                                  - latest level + receivedAt
                                                │
                                      exactly one view
                                      ┌─────────┴─────────┐
                                  BasicHudView        HudView (wave)
```

The controller is the only owner of semantic display lifetime. Views own only
their actors, rendering timers, monitor subscription, transitions, and
view-specific preference readers. This prevents style switching from restarting
a recoverable notice, reviving stale audio, resurrecting an explicitly dismissed
critical error, or leaving an old actor alive.

### View switching transaction

1. Normalize the new preference through the Shell-independent
   `view-selection.js` module (`basic` fallback for missing/unknown values).
2. If unchanged, do nothing.
3. Immediately destroy the old view; never use animated `hide()` for replacement.
4. Construct exactly one new view with an `onDismiss` callback.
5. Replay the currently displayed descriptor only if one should be visible.
6. Replay the latest level with its original `receivedAt` timestamp.
7. Leave held-notice deadline/timer and the dictation service untouched.

If the service vanishes, the controller clears an ordinary active descriptor to
dormant. An already-established held notice remains: recoverable keeps its
absolute deadline and critical keeps its explicit-dismiss requirement. Neither
service loss nor a simultaneous style replacement restarts that lifetime.

### Basic meter

The basic view uses a standard horizontal track and fill. Shared calibration
maps RMS/peak to intensity; the basic mapping converts the existing wave floor
to true zero:

```text
fill = clamp((intensity - FLOOR) / (1 - FLOOR), 0, 1)
```

Only `recording` targets nonzero fill. Silence, stale level, loading,
transcribing, finalizing, notices, and errors target zero. A pure smoothing step
uses fast attack and slower release while repainting long enough to reach zero.
Reduced motion removes decorative easing but retains a legible bounded bar.

### Preferences and packaging

- `metadata.json` names `org.gnome.shell.extensions.myna` as its settings schema.
- `schemas/org.gnome.shell.extensions.myna.gschema.xml` defines enum nicks
  `basic` and `wave`; numeric values are stable and zero-based, matching the
  preferences selector index.
- `prefs.js` implements `ExtensionPreferences.fillPreferencesWindow()` with one
  `Adw.ComboRow`; the separate preferences process never imports Shell modules.
- The Shell process creates settings only during `enable()`, connects
  `changed::hud-style`, reads after connecting, and disconnects during
  `disable()`.
- `gnome-extensions pack/install` compiles local schemas automatically. The
  documented raw-copy development path runs `glib-compile-schemas` in the
  installed `schemas/` directory. Generated `gschemas.compiled` is not tracked.
- `view-selection.js` contains no `gi://` or Shell imports. It normalizes the
  style and chooses between injected constructors, so constructor selection and
  `onDismiss` forwarding are headlessly testable. `view.js` imports the actual
  `BasicHudView`/`HudView` actors and delegates to that pure function.

### Generated artifacts

- [research.md](./research.md): platform and architecture decisions.
- [data-model.md](./data-model.md): preference, controller snapshot, held notice,
  timestamped level, active view, and state transitions.
- [contracts/settings.md](./contracts/settings.md): persistent preference and
  preferences UI contract.
- [contracts/presentation.md](./contracts/presentation.md): controller/view
  interface, switching transaction, level and notice guarantees.
- [quickstart.md](./quickstart.md): runnable headless, package, and GNOME
  acceptance guide.

## Project Structure

### Documentation (this feature)

```text
specs/009-switchable-basic-hud/
├── spec.md
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── settings.md
│   └── presentation.md
├── checklists/
│   └── requirements.md
└── tasks.md                    # generated later by /speckit-tasks
```

### Source Code (repository root)

```text
extensions/myna-shell/
├── extension.js                # settings + service → controller lifecycle
├── indicator-controller.js     # new pure presentation-independent state owner
├── view-selection.js           # new pure style normalization/injected selection
├── view.js                     # Shell adapter + rendering-only contract
├── basic.js                    # new BasicHudView Shell actor
├── basic-logic.js              # new pure energy-bar mapping/smoothing
├── hud.js                      # existing wave view, stripped of notice ownership
├── hud-logic.js                # existing shared geometry/icon/colour helpers
├── dbus.js                     # existing org.myna.Dictation consumer (wire unchanged)
├── states.js                   # existing state descriptor mapping
├── vumeter.js                  # existing calibrated RMS/peak mapping
├── ribbon.js
├── ribbon-paint.js
├── accent.js
├── prefs.js                    # new standard extension preferences page
├── schemas/
│   └── org.gnome.shell.extensions.myna.gschema.xml
├── metadata.json               # gains settings-schema
├── stylesheet.css              # basic bar styles added; wave styles retained
└── test/
    ├── indicator-controller.test.js
    ├── basic.test.js
    ├── view-selection.test.js
    ├── settings.test.js
    ├── lifecycle.test.js       # extended through controller
    └── existing feature-004 tests

.github/workflows/ci.yml        # run Workshop gjs-test + package smoke
.workshop/myna.yaml             # reuse/extend named GJS/package actions if needed
extensions/myna-shell/README.md # install, preferences, both HUDs
README.md                        # point to feature-009 quickstart/current behavior
docs/project-plan.md             # record feature 009/global task status when built
```

**Structure Decision**: Keep the feature entirely in the existing GJS extension.
Add one deep controller module and one independent basic view rather than a
generic widget hierarchy. Do not touch Rust, the D-Bus contract, or snap
packaging. Historical feature-004 artifacts remain as design history; current
documentation may link to feature 009 where its old “single view/no settings”
statements are superseded.

## Post-Design Constitution Re-check

| Principle | Post-design result |
|---|---|
| I | Controller/basic/settings behavior is isolated into pure modules with executable tests; only Shell actor pixels remain manual. PASS under recorded tiering. |
| II | Fake views/settings/clock/scheduler make tests hermetic; installed extension acceptance is unchanged between VM and hardware. PASS. |
| III | Design preserves explicit switch/decay/leak targets and does not touch capture/publisher hot paths. PASS under extension exemption. |
| IV | All dependencies already exist in Workshop; CI invokes the existing action and package smoke. PASS. |
| V | Data model and both contracts carry preference, state, reason, and normalized level only. PASS. |

**Gate result after Phase 1**: PASS. No unresolved clarification or complexity
exception beyond feature 004's already-recorded, platform-required GJS tier.

## Complexity Tracking

No new constitution violation. The existing GJS platform exception is reused;
the new controller reduces rather than increases semantic duplication.
