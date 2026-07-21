# Implementation Plan: GNOME Shell Extension for Myna Dictation UI

**Branch**: `004-gnome-shell-indicator` | **Date**: 2026-07-21 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/004-gnome-shell-indicator/spec.md`

## Summary

Deliver a **GNOME Shell extension** (GJS, in the compositor) that renders a
focus-safe, animated dictation indicator — the "goop" hanging from the top bar
with a Gemini-Live-style activity pulse, an audio-level VU/glow, and colour-coded
model/session states — plus an optional panel toggle. On GNOME/Wayland a normal
client cannot show an always-on-top, non-focus-stealing overlay (survey in
`docs/desktop-injection.md` §2); running inside Mutter is the GNOME-blessed fix.

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
   `DbusIndicator` implementing the existing `Indicator` seam by emitting
   `StateChanged` and updating `State`/`AudioRms`/`AudioPeak` properties, plus a
   `DbusTrigger` feeding `Start`/`Stop`/`Toggle` into the orchestrator's existing
   `Trigger` seam (mirrors `ControlTrigger`). The `AudioStats` `watch` receiver
   (`myna-audio`) already carries the levels; a small pump publishes them.
2. **GNOME Shell extension** (GJS, evaluation-harness-tier — see Constitution
   Check) — subscribes to the interface, drives the St/Clutter goop through its
   states, and exposes state to AT-SPI.

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
  (constitution II).
- Extension (GJS, harness-tier): a headless **contract test** using GJS +
  `Gio` against a stub publisher (or the real one) asserting the state→visual-
  intent mapping and lifecycle (connect/disconnect/unknown-state) via a
  testable pure mapping module extracted from the actor code; plus a manual
  **on-hardware acceptance** (install the extension, run `myna-desktop`, drive
  each state, observe the goop + focus-safety). The GJS suite is scaffolding for
  the harness tier (see Constitution Check), not gated by TDD.

**Target Platform**: Ubuntu Desktop 26.04+ on Wayland, GNOME Shell 50/51; session
D-Bus present. Older GNOME and non-GNOME desktops are out of scope (they keep the
`NotifyIndicator` path).

**Project Type**: Desktop — a Rust workspace addition (the publisher, in
`myna-desktop`) plus a **new top-level GJS artifact** `extensions/myna-shell/`
(GNOME Shell extension bundle: `metadata.json`, `extension.js`, modules, CSS).

**Performance Goals** (inherited from feature 003 / UD129, pinned as watermarks —
constitution III): indicator visible within the activation-latency target
(≈100–200 ms) after `StateChanged(recording)`; goop animations sustain ≈60 fps
and never block the compositor; audio-level updates at ~15–20 Hz with the VU
decaying to floor within a bounded window (~300 ms) on a stale stream;
`StateChanged` → visual update < 50 ms; no capture-path regression (publisher
adds only a `watch` read + a signal emit per state change).

**Constraints**: focus-safe (never take key focus — the entire point); push-to-talk
(no overlay while idle); **privacy** — the interface and the indicator carry state
+ level only, never transcript text, and nothing is persisted or logged by default
(constitution V); offline (no network); the publisher must not regress the capture
path; the extension must release all actors/timers/D-Bus subscriptions on `disable`
and re-init cleanly across Shell restart / relogin.

**Scale/Scope**: one new Rust module pair in `myna-desktop` (`DbusIndicator` +
`DbusTrigger`, each behind the existing seams, each with a fake-bus test) wiring a
`--dbus` activation mode into the binary; one new GJS extension bundle (~4–6 JS
modules + CSS + `metadata.json`); one D-Bus contract (`org.myna.Dictation`) shared
by both; one env-gated Rust integration suite; one GJS contract test; a manual
on-hardware acceptance.

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
| I. Red-Green TDD (post-ratification) | Publisher: `DbusIndicator` state→signal mapping, property snapshots, and `DbusTrigger` edge dedup land test-first behind a fake-bus seam; the contract table in `contracts/dbus-interface.md` is encoded as executable tests before code. Extension: harness-tier — the pure state→visual-intent mapping gets a GJS contract test, but actor/animation code is exercised by the manual acceptance, not test-first. | PASS (publisher); EXEMPT (extension, harness-tier) |
| II. Integration-Test Readiness | Publisher boundary is the `zbus` object behind the existing seams; hermetic tests use a fake bus, real behaviour in one `MYNA_DBUS_TESTS`-gated suite runnable on VM and hardware unchanged. Extension acceptance runs on a GNOME session (VM with a Wayland GNOME session, or hardware) via the same D-Bus contract. | PASS (by design) |
| III. Performance Watermarks | `StateChanged`→visible, level-update cadence, and animation frame-rate targets are declared (Technical Context) with tolerances; the publisher's per-state overhead is measured as a Rust watermark (reuses feature-002/003 capture baselines — no capture-path change). Extension fps is a manual observation (harness-tier exemption). | PASS (publisher); EXEMPT (extension) |
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
│   │   │   ├── mod.rs         #   Indicator seam (UNCHANGED)
│   │   │   ├── dbus.rs        #   NEW: DbusIndicator — emits org.myna.Dictation
│   │   │   │                  #        StateChanged + State/AudioRms/AudioPeak
│   │   │   └── notify.rs …    #   UNCHANGED fallback (NotifyIndicator)
│   │   ├── shortcut/
│   │   │   ├── mod.rs         #   Trigger seam (UNCHANGED)
│   │   │   └── dbus.rs        #   NEW: DbusTrigger — Start/Stop/Toggle → edges
│   │   ├── dbus/              #   NEW: shared org.myna.Dictation object + zbus glue
│   │   │   └── mod.rs         #     the served interface; level pump from AudioStats
│   │   └── bin/
│   │       └── myna-desktop.rs #   + `--dbus` mode: serve org.myna.Dictation,
│   │                          #     use DbusIndicator (+ DbusTrigger) with fallback
│   └── tests/
│       ├── dbus_indicator.rs  #   hermetic: state→signal/property mapping (fake bus)
│       └── dbus_hw.rs         #   env-gated (MYNA_DBUS_TESTS): real session-bus round-trip
└── Cargo.toml                 # UNCHANGED members (no new crate; no new deps)

extensions/                    # NEW top-level: GJS artifacts (non-Rust, harness-tier)
└── myna-shell/                # the GNOME Shell extension bundle
    ├── metadata.json          #   uuid, shell-version [50, 51], name, settings-schema (none)
    ├── extension.js           #   enable()/disable(): wire proxy ↔ Indicator actor
    ├── dbus.js                #   Gio.DBusProxy for org.myna.Dictation (connect/reconnect)
    ├── indicator.js           #   the goop St.DrawingArea/St.Widget + PanelMenu button
    ├── states.js              #   PURE: dictation-state → visual-intent (contract-tested)
    ├── vumeter.js             #   level → glow/bar intensity + stale decay
    ├── stylesheet.css         #   state colours, high-contrast, animation classes
    └── test/
        └── states.test.js     #   GJS contract test of states.js + lifecycle (harness-tier)

docs/
└── desktop-injection.md       # UPDATED: §2 "no sanctioned overlay" → this extension is
                               #   the GNOME answer; NotifyIndicator stays the fallback
```

**Structure Decision**: The shipped, testable half (state/level publishing,
activation) lives in Rust in the existing `myna-desktop` crate behind the seams
that already exist — a `DbusIndicator` (new `Indicator` backend) and a
`DbusTrigger` (new `Trigger` backend, sibling to `ControlTrigger`), plus a small
`dbus` module that serves `org.myna.Dictation` and pumps `AudioStats` levels. No
new crate and no new Rust dependency (reusing vendored `zbus`). The in-compositor
UI lives in a **new top-level `extensions/myna-shell/`** GJS bundle because a
GNOME Shell extension cannot be anything but GJS — quarantined from the Rust
workspace, harness-tier, with its logic factored into a pure `states.js` so the
state→visual mapping is unit-testable without a running Shell. The D-Bus contract
(`org.myna.Dictation`) is the single seam between the two, defined once in
`contracts/dbus-interface.md`.

## Complexity Tracking

> Only rows that need constitutional justification.

| Violation / Risk | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| **GJS (non-Rust) extension** — a shipped-adjacent UI component not in Rust | GNOME Shell extensions run *inside Mutter* and MUST be GJS/Clutter/St; there is no Rust option for in-compositor UI. This is the only sanctioned way to show a focus-safe always-on-top overlay on GNOME (survey `docs/desktop-injection.md` §2). | (a) A Rust `gtk4-layer-shell` overlay — layer-shell is *not implemented by Mutter/GNOME* (feature 003 R6), so it works on wlroots/KDE only, not the primary target; (b) staying on `NotifyIndicator` — cannot show a persistent live goop / VU / model-loading glow (the feature's whole point). GJS is unavoidable; it is quarantined to `extensions/` and treated as harness-tier. |
| **Extension exempt from strict TDD + watermark baselines** | GJS actor/animation/compositor code cannot be meaningfully unit-tested without a running Shell, and fps is a compositor-observed property; mirrors the constitution's Python-testbed carve-out for evaluation-harness scaffolding. | Requiring test-first coverage of Clutter animations would test a mock of the Shell, not the integration where bugs live (constitution II rationale). Coverage instead splits: the pure `states.js` mapping gets a GJS contract test; the compositor behaviour gets a manual on-hardware acceptance. The Rust publisher — the shipped, logic-bearing half — keeps full TDD. |
| **New top-level `extensions/` tree outside the Cargo workspace** | The GJS bundle has no place in a Rust workspace and follows GNOME's fixed extension layout (`metadata.json` + ESM modules at the bundle root). | Nesting it under a crate would fight both `cargo` and the GNOME extension loader (which expects the bundle as-is under `~/.local/share/gnome-shell/extensions/<uuid>/`). A sibling top-level tree keeps each toolchain clean. |
| **New Workshop deps** (session D-Bus for the gated suite; GJS + a GNOME Shell session for the extension acceptance) | Constitution IV mandates the Workshop definition gain deps in the introducing PR. | Deferring violates IV; scoped as a foundational task extending `.workshop/myna.yaml` (the `desktop` SDK from feature 003 likely already supplies D-Bus; GJS/gnome-shell are the additions). |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research + data-model + contracts + quickstart):

- **I. TDD** — `contracts/dbus-interface.md` and `contracts/publisher.md` are
  row-per-guarantee tables encoded as hermetic Rust tests (state→signal,
  property snapshot, trigger dedup) before code; the extension's pure mapping is
  contract-tested in GJS, actor/animation behaviour deferred to the manual
  acceptance (harness-tier). PASS (publisher) / EXEMPT (extension).
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
  network; capture path and buffers unchanged. PASS.

No principle is violated by the design; the tracked items are the GJS
harness-tier tiering (Complexity Tracking, accepted) and the Workshop deps (IV),
which this feature is the correct occasion to add.
