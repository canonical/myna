# Implementation Plan: GNOME Shell Extension for Myna Dictation UI

**Branch**: `004-gnome-shell-indicator` | **Date**: 2026-07-21 (HUD redesign: 2026-07-30) | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/004-gnome-shell-indicator/spec.md`

## Summary

Deliver a **GNOME Shell extension** (GJS, in the compositor) that renders a
focus-safe dictation **HUD pill** — bottom-center of the screen, styled like
GNOME's own volume/brightness OSD, showing a segmented, colour-zoned
(green/yellow/red) VU meter calibrated to real speech levels, a content-free
status label, and a mic/mic-slash icon — plus
an optional panel toggle. On GNOME/Wayland a normal client cannot show an
always-on-top, non-focus-stealing overlay (survey in
`docs/desktop-injection.md` §2); running inside Mutter is the GNOME-blessed fix.
**(2026-07-30 revision: the original "goop" ribbon/blob design — see R6/R12 in
research.md — is replaced by this bottom-center HUD pill; the `RibbonView`
implementation is deleted, not kept as an alternate.)**

The indicator now also distinguishes two problem severities: a **recoverable**
issue (e.g. "no speech detected" — a session that completed successfully but
captured nothing) renders as a non-blocking notice that auto-dismisses and
never blocks a new session, while a **critical** error (e.g. microphone
unavailable) renders as a persistent notice with an explicit dismiss (×)
control that is clickable but never keyboard-focusable (preserving the
non-focus-stealing invariant). This severity is an interim, client-inferred
classification pending T31/T62's wire-level error taxonomy (spec Assumptions).

The extension is **pure UI**: it consumes dictation state + audio levels from the
existing `myna-desktop` process over a new **session-bus D-Bus interface**
(`org.myna.Dictation`) and MAY call `Toggle`/`Start`/`Stop`; it never captures
audio, transcribes, or injects text (IBus injection stays in `myna-desktop`,
feature 003). The D-Bus **emitting side** is added to `myna-desktop` (Rust);
the **consuming side** is the extension (GJS). On GNOME the extension becomes the
preferred indicator surface — feature 003's `NotifyIndicator` remains the
fallback when the extension is absent (spec FR-020 / FR-023).

Two deliverables, one contract between them:
1. **`myna-desktop` D-Bus publisher** (Rust, TDD, shipped component) — a
   `DbusIndicator` implementing the existing `Indicator` seam by updating
   `State`/`ErrorMessage`/`AudioRms`/`AudioPeak` properties (each update pushed
   via the standard `PropertiesChanged`), plus a
   `DbusTrigger` feeding `Start`/`Stop`/`Toggle` into the orchestrator's existing
   `Trigger` seam (mirrors `ControlTrigger`). The `AudioStats` `watch` receiver
   (`myna-audio`) already carries the levels; a small pump publishes them.
   **(2026-07-30 addition)**: `IndicatorState::Error` gains a `recoverable: bool`
   field; a session that completes with an empty/blank transcript publishes the
   new `notice` wire state (recoverable) instead of `idle`, via a shared
   `completion_indicator_state()` helper used by both the live per-event path
   and the finalize-block safety net so the two can never disagree (R13).
2. **GNOME Shell extension** (GJS, evaluation-harness-tier — see Constitution
   Check) — subscribes to the interface, drives the bottom-center HUD pill
   through its states/severities, and exposes state to AT-SPI.


## Technical Context

**Language/Version**:
- `myna-desktop` publisher: Rust (stable, workspace edition 2021, `rust-version = 1.75`).
- Extension: **GJS** (GNOME JavaScript / SpiderMonkey) targeting GNOME Shell 50
  and 51 — the platform-mandated language for in-compositor UI (no Rust option).

**Primary Dependencies**:
- Publisher side: `zbus` (5.x, **already vendored**) — serves the
  `org.myna.Dictation` object on the session bus; reuses the orchestrator's
  `Trigger`/`TextSink`/`Indicator` seams and `myna-audio`'s `AudioStats`
  `watch::Receiver` (both already present). No new Rust crates.
- Extension side: GNOME Shell platform modules only — `St`, `Clutter`, `GObject`,
  `Gio` (D-Bus), `GLib`, `PanelMenu`/`Main` (`resource:///org/gnome/shell/...`),
  and `Adwaita`/theme CSS. `Gio.DBusProxy` consumes `org.myna.Dictation`. No npm
  or bundler; ESM modules per the GNOME 45+ extension format.

**Storage**: N/A. No settings store in scope (no model/mic/language picker —
Out of Scope). The extension keeps only in-memory transient state (current
dictation state + last level); nothing persisted. Audio is never touched by
either deliverable.

**Testing**:
- Publisher (Rust, TDD): hermetic `cargo test` over the `DbusIndicator`
  state→signal mapping and the `DbusTrigger` edge production, driven by a
  **fake bus** boundary (in-memory) — no session bus required; plus an
  **env-gated integration suite** (`MYNA_DBUS_TESTS=1`) that stands the object on
  a real session bus (or `dbus-run-session`) and asserts a client sees the
  signals/properties, runnable identically on the desktop VM and hardware
  (constitution II). **(2026-07-30)**: extended to cover the new
  `recoverable`/`notice` split — `completion_indicator_state()` unit tests
  (empty vs. non-empty transcript), `map_state`'s new `notice` arm, and the
  6-file mechanical ripple from the `IndicatorState::Error` field addition
  (each site gets a red-green pair per constitution I, even where behavior is
  unchanged — e.g. `gtk.rs`/`notify.rs` ignoring the new field).
- Extension (GJS, harness-tier): a headless **contract test** using GJS +
  `Gio` against a stub publisher (or the real one) asserting the state→visual-
  intent mapping and lifecycle (connect/disconnect/unknown-state) via a
  testable pure mapping module extracted from the actor code; plus a manual
  **on-hardware acceptance** (install the extension, run `myna-desktop`, drive
  each state, observe the HUD pill + focus-safety). The GJS suite is scaffolding
  for the harness tier (see Constitution Check), not gated by TDD. **(2026-07-30)**:
  extended with contract tests for the replace-in-place/restart-timer behavior
  (R15) and the dismiss control's reactive-but-non-focusable property (FR-007c).

**Target Platform**: Ubuntu Desktop 26.04+ on Wayland, GNOME Shell 50/51; session
D-Bus present. Older GNOME and non-GNOME desktops are out of scope (they keep the
`NotifyIndicator` path).

**Project Type**: Desktop — a Rust workspace addition (the publisher, in
`myna-desktop`) plus a **new top-level GJS artifact** `extensions/myna-shell/`
(GNOME Shell extension bundle: `metadata.json`, `extension.js`, modules, CSS).

**Performance Goals** (inherited from feature 003 / UD129, pinned as watermarks —
constitution III): indicator visible within the activation-latency target
(≈100–200 ms) after `State=recording` is published; HUD animations (bar meter,
appear/dismiss transitions) sustain ≈60 fps and never block the compositor;
audio-level updates at ~15–20 Hz with the bar meter decaying to floor within a
bounded window (~300 ms) on a stale stream; state push → visual update < 50 ms;
no capture-path regression (publisher adds only a `watch` read + a property set
per state change, unchanged by the severity split).

**Constraints**: focus-safe (never take key focus — the entire point, including
the new dismiss (×) control which is reactive but never focusable, FR-007c);
push-to-talk (no overlay while idle); **privacy** — the interface and the
indicator carry state + level only, never transcript text, and nothing is
persisted or logged by default (constitution V) — the recoverable/critical
classification is itself content-free (a severity label, not the transcript
that triggered it); offline (no network); the publisher must not regress the
capture path; the extension must release all actors/timers/D-Bus subscriptions
on `disable` and re-init cleanly across Shell restart / relogin.

**Scale/Scope**: one new Rust module pair in `myna-desktop` (`DbusIndicator` +
`DbusTrigger`, each behind the existing seams, each with a fake-bus test) wiring a
`--dbus` activation mode into the binary; **(2026-07-30)** one field addition to
the shared `IndicatorState::Error` variant (mechanical ripple across 6 files —
`indicator/{dbus,gtk,notify}.rs`, `controller.rs`, `tests/{dbus_indicator,
controller}.rs` — 0 new Rust dependencies, 0 new Workshop deps); one GJS extension
bundle (~4–6 JS modules + CSS + `metadata.json`, with `indicator.js` replaced by
`hud.js`); one D-Bus contract (`org.myna.Dictation`, gaining one additive `State`
value) shared by both; one env-gated Rust integration suite; one GJS contract
test; a manual on-hardware acceptance.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.3.0. This feature spans **two tiers**:

- The **`myna-desktop` D-Bus publisher** is a *shipped Rust system component* —
  all principles apply in full (TDD, integration-readiness, watermarks, Workshop,
  privacy).
- The **GJS extension** is an in-compositor UI that *cannot* be Rust (platform
  constraint of GNOME Shell). It is treated as **evaluation-harness-tier
  scaffolding** analogous to the Python testbed carve-out (Technology &
  Environment Constraints): exempt from the Rust-language rule, the strict
  test-first TDD requirement, and checked-in performance-watermark baselines.
  It still MUST honour the privacy and offline invariants (V) and is covered by a
  GJS contract test + a manual on-hardware acceptance. This tiering is recorded in
  Complexity Tracking; it is the correct occasion to note whether the constitution
  should name GJS UI shims explicitly (follow-up, not blocking).

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD (post-ratification) | Publisher: `DbusIndicator` state→signal mapping, property snapshots, and `DbusTrigger` edge dedup land test-first behind a fake-bus seam; the contract table in `contracts/dbus-interface.md` is encoded as executable tests before code. **(2026-07-30)** The `IndicatorState::Error{recoverable}` field addition and `completion_indicator_state()` helper land test-first across all 6 touched files, including the mechanical arms in `gtk.rs`/`notify.rs` that assert behavior is unchanged. Extension: harness-tier — the pure state→visual-intent mapping gets a GJS contract test, but actor/animation code is exercised by the manual acceptance, not test-first. | PASS (publisher); EXEMPT (extension, harness-tier) |
| II. Integration-Test Readiness | Publisher boundary is the `zbus` object behind the existing seams; hermetic tests use a fake bus, real behaviour in one `MYNA_DBUS_TESTS`-gated suite runnable on VM and hardware unchanged. Extension acceptance runs on a GNOME session (VM with a Wayland GNOME session, or hardware) via the same D-Bus contract. | PASS (by design) |
| III. Performance Watermarks | state-push→visible, level-update cadence, and animation frame-rate targets are declared (Technical Context) with tolerances; the publisher's per-state overhead is measured as a Rust watermark (reuses feature-002/003 capture baselines — no capture-path change). Extension fps is a manual observation (harness-tier exemption). | PASS (publisher); EXEMPT (extension) |
| IV. Workshop-Based Dev Environment | New test/runtime deps must land in `.workshop/myna.yaml`: a **session D-Bus** for the gated publisher suite (likely already present via the `desktop` SDK from feature 003) and, for the extension acceptance, **GJS + a GNOME Shell session** (`gnome-shell`, `gjs`). Scoped as a foundational task; the extension bundle needs no build toolchain (pure GJS). | GATED — tracked |
| V. Privacy-First, Offline-First | The interface exposes state + normalized level only — never transcript text; the extension renders/logs/persists no content and captures no audio; no network on either side; audio buffers unchanged (publisher only reads the existing `AudioStats` `watch`). | PASS (by design) |

**Post-Phase-1 re-check**: see the end of this file — re-evaluated after the
design artifacts; no new violations introduced.

## Project Structure

### Documentation (this feature)

```text
specs/004-gnome-shell-indicator/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── dbus-interface.md #   org.myna.Dictation (the shared contract)
│   ├── publisher.md      #   DbusIndicator + DbusTrigger seam guarantees (Rust)
│   └── extension.md      #   extension lifecycle + state→visual-intent guarantees (GJS)
├── checklists/
│   └── requirements.md  # from /speckit-specify
└── tasks.md             # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
client/
├── myna-audio/                # UNCHANGED — AudioStats watch::Receiver reused (levels)
├── myna-orchestrator/         # UNCHANGED seams reused (Trigger / TextSink / Indicator)
├── myna-desktop/              # EXTENDED — the D-Bus publisher (shipped Rust)
│   ├── src/
│   │   ├── indicator/
│   │   │   ├── mod.rs         #   EXTENDED: IndicatorState::Error gains `recoverable: bool`
│   │   │   ├── dbus.rs        #   EXTENDED: DbusIndicator — publishes
│   │   │   │                  #        State/ErrorMessage/AudioRms/AudioPeak;
│   │   │   │                  #        map_state gains the notice/error split
│   │   │   ├── gtk.rs         #   MECHANICAL: destructure update, behavior unchanged
│   │   │   └── notify.rs      #   MECHANICAL: destructure update, behavior unchanged
│   │   ├── shortcut/
│   │   │   ├── mod.rs         #   Trigger seam (UNCHANGED)
│   │   │   └── dbus.rs        #   NEW: DbusTrigger — Start/Stop/Toggle → edges
│   │   ├── dbus/              #   NEW: shared org.myna.Dictation object + zbus glue
│   │   │   └── mod.rs         #     the served interface; level pump from AudioStats
│   │   ├── controller.rs      #   EXTENDED: completion_indicator_state() helper used by
│   │   │                      #     both the live event_to_indicator() Done arm and the
│   │   │                      #     finalize-block SessionOutcome::Completed handler
│   │   └── bin/
│   │       └── myna-desktop.rs #   + `--dbus` mode: serve org.myna.Dictation,
│   │                          #     use DbusIndicator (+ DbusTrigger) with fallback
│   └── tests/
│       ├── dbus_indicator.rs  #   hermetic: state→signal/property mapping (fake bus);
│       │                      #     EXTENDED with notice-state cases
│       ├── controller.rs      #   EXTENDED: empty-transcript → notice test cases
│       └── dbus_hw.rs         #   env-gated (MYNA_DBUS_TESTS): real session-bus round-trip
└── Cargo.toml                 # UNCHANGED members (no new crate; no new deps)

extensions/                    # NEW top-level: GJS artifacts (non-Rust, harness-tier)
└── myna-shell/                # the GNOME Shell extension bundle
    ├── metadata.json          #   uuid, shell-version [50, 51], name, settings-schema (none)
    ├── extension.js           #   enable()/disable(): wire proxy ↔ Indicator actor
    ├── dbus.js                #   Gio.DBusProxy for org.myna.Dictation (connect/reconnect)
    │                          #     UNCHANGED by this redesign (no wire member renamed)
    ├── hud.js                 #   NEW — replaces indicator.js: bottom-center HUD pill
    │                          #     (St.Widget styled like GNOME's OSD, not the internal
    │                          #     OsdWindow class), segmented bar meter, mic/mic-slash
    │                          #     icon, dismiss (×) control (reactive/non-focusable),
    │                          #     replace-in-place + restart-timer notice/error logic
    ├── indicator.js           #   REMOVED — RibbonView deleted, not kept as alt view
    ├── states.js              #   RESHAPED: descriptor gains `severity:
    │                          #     'recoverable'|'critical'|null` replacing `isError` bool
    ├── vumeter.js             #   UNCHANGED: level→intensity + stale-decay still feeds bars
    ├── stylesheet.css         #   REVISED: pill/bar/icon styling replacing ribbon/goop
    └── test/
        ├── states.test.js     #   UPDATED for the new descriptor shape
        ├── hud.test.js        #   NEW — replaces ribbon-specific coverage: replace-in-place,
        │                      #     restart-timer, dismiss non-focusable property
        └── lifecycle.test.js  #   UNCHANGED (dbus.js untouched by this redesign)

docs/
└── desktop-injection.md       # UPDATED: §2 "no sanctioned overlay" → this extension is
                               #   the GNOME answer; NotifyIndicator stays the fallback
```

**Structure Decision**: The shipped, testable half (state/level publishing,
activation) lives in Rust in the existing `myna-desktop` crate behind the seams
that already exist — a `DbusIndicator` (new `Indicator` backend) and a
`DbusTrigger` (new `Trigger` backend, sibling to `ControlTrigger`), plus a small
`dbus` module that serves `org.myna.Dictation` and pumps `AudioStats` levels.
**(2026-07-30)** The recoverable/critical severity split extends the existing
`IndicatorState::Error` variant with one field rather than adding a new
top-level variant or a separate D-Bus property — this is a mechanical,
compiler-forced ripple across 6 files, not a new module, and leaves feature
003's GTK/Notify indicators behaviorally unchanged. No new crate and no new
Rust dependency (reusing vendored `zbus`). The in-compositor UI lives in a
**new top-level `extensions/myna-shell/`** GJS bundle because a GNOME Shell
extension cannot be anything but GJS — quarantined from the Rust workspace,
harness-tier, with its logic factored into a pure `states.js` so the
state→visual mapping is unit-testable without a running Shell. The D-Bus contract
(`org.myna.Dictation`) is the single seam between the two, defined once in
`contracts/dbus-interface.md`; the severity split is realized as one additive
`State` value (`notice`), not a new property, so an unpatched extension build
degrades to the existing neutral "active" treatment (FR-008) rather than
breaking.

## Complexity Tracking

> Only rows that need constitutional justification.

| Violation / Risk | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| **GJS (non-Rust) extension** — a shipped-adjacent UI component not in Rust | GNOME Shell extensions run *inside Mutter* and MUST be GJS/Clutter/St; there is no Rust option for in-compositor UI. This is the only sanctioned way to show a focus-safe always-on-top overlay on GNOME (survey `docs/desktop-injection.md` §2). | (a) A Rust `gtk4-layer-shell` overlay — layer-shell is *not implemented by Mutter/GNOME* (feature 003 R6), so it works on wlroots/KDE only, not the primary target; (b) staying on `NotifyIndicator` — cannot show a persistent live goop / VU / model-loading glow (the feature's whole point). GJS is unavoidable; it is quarantined to `extensions/` and treated as harness-tier. |
| **Extension exempt from strict TDD + watermark baselines** | GJS actor/animation/compositor code cannot be meaningfully unit-tested without a running Shell, and fps is a compositor-observed property; mirrors the constitution's Python-testbed carve-out for evaluation-harness scaffolding. | Requiring test-first coverage of Clutter animations would test a mock of the Shell, not the integration where bugs live (constitution II rationale). Coverage instead splits: the pure `states.js` mapping gets a GJS contract test; the compositor behaviour gets a manual on-hardware acceptance. The Rust publisher — the shipped, logic-bearing half — keeps full TDD. |
| **New top-level `extensions/` tree outside the Cargo workspace** | The GJS bundle has no place in a Rust workspace and follows GNOME's fixed extension layout (`metadata.json` + ESM modules at the bundle root). | Nesting it under a crate would fight both `cargo` and the GNOME extension loader (which expects the bundle as-is under `~/.local/share/gnome-shell/extensions/<uuid>/`). A sibling top-level tree keeps each toolchain clean. |
| **New Workshop deps** (session D-Bus for the gated suite; GJS + a GNOME Shell session for the extension acceptance) | Constitution IV mandates the Workshop definition gain deps in the introducing PR. | Deferring violates IV; scoped as a foundational task extending `.workshop/myna.yaml` (the `desktop` SDK from feature 003 likely already supplies D-Bus; GJS/gnome-shell are the additions). |
| **(2026-07-30) `IndicatorState::Error` field addition ripples across 6 files** — `indicator/{mod,dbus,gtk,notify}.rs`, `controller.rs`, plus 2 test files, all outside this feature's original scope (feature 003's GTK/Notify indicators) | The recoverable/critical severity distinction (spec FR-007/FR-017) needs to reach `DbusIndicator::map_state` without fabricating a fake "error" transition for what is actually a successful, empty-transcript completion; the cleanest way to compute that once and share it across both the live-event and finalize-block call sites is a shared helper reached through the existing shared enum. | (a) A wholly new `IndicatorState::Notice(String)` top-level variant — same 6-file ripple, no smaller, and reads as a disconnected concept rather than an error severity; (b) a side-channel bypassing the `Indicator` trait object (e.g. `Any`-downcasting to a `DbusIndicator`-only method) — non-idiomatic Rust, breaks the trait-object seam the whole indicator system relies on; (c) a separate `ErrorSeverity` D-Bus property with a synthesized `error` transition for the empty-transcript case — semantically wrong (that path is a *success*, not an error) and would give GTK/Notify indicators a real error toast for a non-error event unless separately special-cased. The field addition is the smallest correct option; GTK/Notify indicators' match arms ignore the new field, so their rendering is provably unchanged (test-covered). |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research + data-model + contracts + quickstart):

- **I. TDD** — `contracts/dbus-interface.md` and `contracts/publisher.md` are
  row-per-guarantee tables encoded as hermetic Rust tests (state→signal,
  property snapshot, trigger dedup) before code; the extension's pure mapping is
  contract-tested in GJS, actor/animation behaviour deferred to the manual
  acceptance (harness-tier). **(2026-07-30)** The `notice`/`error` severity split
  (C10/C11) and the `completion_indicator_state()` helper land test-first, as
  does each of the 6 mechanically-touched files (including the "unchanged
  behavior" assertions in `gtk.rs`/`notify.rs`). PASS (publisher) / EXEMPT
  (extension).
- **II. Integration readiness** — publisher hermetic on a fake bus; real
  session-bus round-trip in the `MYNA_DBUS_TESTS` suite, identical on VM and
  hardware; the extension acceptance runs against the same contract on a GNOME
  session. PASS.
- **III. Watermarks** — publisher per-state overhead + level-pump cadence are
  Rust watermarks with tolerances; capture-path baselines inherited unchanged
  from features 002/003. Extension fps is a manual observation (exempt). PASS.
- **IV. Workshop** — the one open gate: session D-Bus (gated suite) + GJS/
  gnome-shell (acceptance) declared in `.workshop/myna.yaml`. Scheduled as a
  foundational task. GATED until it lands.
- **V. Privacy/offline** — interface + indicator carry state + normalized level
  only; no transcript text crosses the bus or is rendered/logged/persisted; no
  network; capture path and buffers unchanged. **(2026-07-30)** The severity
  classification itself is content-free (a `recoverable`/`critical` label, never
  the transcript that triggered it); the empty-transcript check happens
  server-side in `myna-desktop` and only its boolean outcome crosses the bus as
  a `State` value. PASS.

No principle is violated by the design; the tracked items are the GJS
harness-tier tiering (Complexity Tracking, accepted), the Workshop deps (IV),
which this feature is the correct occasion to add, and the newly-tracked
`IndicatorState::Error` field ripple (2026-07-30, Complexity Tracking, accepted).
