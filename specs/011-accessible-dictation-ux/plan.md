# Implementation Plan: Accessible Dictation UX

**Branch**: `011-accessible-dictation-ux` | **Date**: 2026-08-27 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/011-accessible-dictation-ux/spec.md`

## Summary

Make every dictation-state signal (idle → loading → listening → transcribing →
finishing → notice/failure) perceivable through more than sight: proactive
AT-SPI announcements reaching speech and braille from both shipped indicator
surfaces (the `myna-shell` GNOME Shell HUD and `myna-desktop`'s indicators —
`DbusIndicator`/`NotifyIndicator` and the opt-in `GtkIndicator`), a
programmatically-queryable accessible name/description at any moment (not just
at transitions), optional short sound cues with a non-regression WER bound,
honouring of the desktop's reduced-motion/high-contrast/text-scale/forced-colour
preferences, non-chorded/non-held activation that survives sticky-keys/slow-keys/
autorepeat, plain-language cross-surface failure messages, and a screen-reader-
readable terminal client (`myna-cli`). No settings UI, no localisation, and no
change to the text-injection path are in scope (spec Assumptions).

The technical core is one new primitive shared by every surface: a **content-free
AT-SPI "Announcement" event** (the same underlying AT-SPI2 protocol member GTK
4.14's `gtk_accessible_announce()` uses) emitted directly onto the accessibility
bus — from Rust via the `atspi` crate (`myna-desktop`, headless: no GTK
dependency required for the shipped `DbusIndicator`/`NotifyIndicator` path), from
GJS via a raw `Gio.DBusConnection` to `org.a11y.Bus` (`myna-shell`, which has no
toolkit-level convenience call of its own), and via `gtk_accessible_announce()`
directly for the opt-in `GtkIndicator`. One shared `AccessibilityAnnouncer` seam
(trait in Rust; a small pure module in GJS) makes the emission unit-testable
without a real bus, with a hardware/service-gated integration suite exercising
the genuine `org.a11y` bus — the same red-green split already used for
`DbusIndicator` in feature 004.

## Technical Context

**Language/Version**:
- `myna-desktop`, `myna-cli`: Rust (stable, workspace edition 2021, `rust-version`
  per workspace).
- `extensions/myna-shell`: GJS (GNOME JavaScript), targeting the GNOME Shell
  versions already supported by feature 004.

**Primary Dependencies**:
- New: `atspi` (pure-Rust, zbus-based AT-SPI2 protocol crate) in `myna-desktop`
  for the headless announcer (`DbusIndicator`/`NotifyIndicator` path) — connects
  to `org.a11y.Bus`, registers a minimal always-present accessible object for
  the dictation session, and emits `Announcement` events plus exposes
  name/description/state for FR-001's on-demand query. No GTK dependency needed
  for this path.
- Existing, reused: `gtk4` (`ui-gtk` feature) — bump the declared GTK feature
  level to expose `gtk_accessible_announce()` (stable since GTK 4.14) on
  `GtkIndicator`; `zbus` (already vendored, `atspi` builds on it); `notify-rust`
  (existing toast path, extended per FR-025/026 persistence semantics);
  `pipewire` (already vendored in `myna-audio`) reused for sound-cue playback
  rather than adding a new audio-output dependency (e.g. `libcanberra`) — see
  research.md R2.
- Extension side: stock `Gio`/`GLib` (already imported for the `org.myna.Dictation`
  D-Bus proxy) for the raw `org.a11y.Bus` announcement call and for reading
  `org.gnome.desktop.a11y`/`org.gnome.desktop.interface` GSettings
  (reduced-motion, high-contrast, text-scale, accent-color — accent/reduced-motion
  reading already exists from feature 004's `accent.js`).

**Storage**: The `org.myna.dictation` GSettings schema
(`client/data/glib-2.0/schemas/org.myna.dictation.gschema.xml`,
`myna_core::settings::Store`) — established by
`feat(client): Move settings into GSettings, with a snap-config default`,
landed on `integration-220627` after this feature's initial planning pass and
picked up on rebase. This resolves what was an open dependency
(project-plan T54, "no shared store today") into a concrete answer: this
feature adds three keys to that existing schema —
`announcement-verbosity` (enum, default `all-transitions`),
`sound-cues-enabled` (boolean, default `true`), and
`silence-auto-stop-seconds` (uint, default `15`) — rather than inventing a
separate store or waiting on the still out-of-scope settings-*UI* feature
(spec Assumptions: the UI is separate; the store it will eventually write
through already exists). `myna-desktop`'s `preferences::GSettingsPreferences`
reads it via `myna_core::settings::Settings::load()`, falling back to the
identical FR-004-mandated defaults when the schema is not installed
(`Settings::default()`) — the same fallback contract every other key in that
store already has. The Shell extension and `myna-cli` read the same schema
directly, giving FR-004/FR-010/FR-018's cross-process consistency requirement
a real, shared answer rather than a named-but-unsolved dependency.

**Testing**:
- `myna-desktop`/`myna-cli` (Rust, TDD, constitution I): hermetic `cargo test`
  against a fake/in-memory `AccessibilityAnnouncer` and the coverage-matrix/
  contrast-check logic; an env-gated integration suite (`MYNA_ATSPI_TESTS=1`,
  mirroring the existing `MYNA_DBUS_TESTS`/`MYNA_PIPEWIRE_TESTS` convention)
  against a real `org.a11y` bus (e.g. `at-spi-bus-launcher` in the Workshop
  desktop SDK or a nested `dbus-run-session`).
- `extensions/myna-shell` (GJS, harness-tier per feature 004 precedent): the
  pure announcement-formatting/coalescing module and the state→channel
  coverage-matrix data get GJS contract tests (jasmine-style, `test/*.test.js`);
  the actual `org.a11y.Bus` call and live compositor behaviour are exercised by
  the manual acceptance protocol (FR-030/031) — GNOME Shell's nested-compositor
  mode is unavailable on Wayland (spec Edge Cases), so full end-to-end AT-SPI-tree
  assertions for the Shell HUD specifically are out of automated reach; this gap
  is recorded, not silently assumed closed (FR-030).
- Coverage-matrix / contrast / colour-only-encoding regression gates (FR-032):
  a single checked-in data file (`data-model.md`'s coverage matrix) validated by
  a hermetic Rust test and a hermetic GJS test, each reading the same file so
  the two surfaces cannot silently diverge.

**Target Platform**: Ubuntu Desktop (current LTS+), GNOME Shell/Wayland primary
target (per constitution); the strictly confined `myna` snap is the packaged
form the acceptance gates (FR-033/SC-009) must also pass against.

**Project Type**: Desktop — extends three existing components (`myna-desktop`,
`myna-cli`, `extensions/myna-shell`); no new top-level project.

**Performance Goals**: state-change → AT-SPI announcement ≤ 500 ms (SC-003);
announcement emission adds no measurable overhead to the capture/transcribe/
inject path when no AT is listening (FR-007); sound-cue playback does not
degrade WER by more than 0.5 pp vs. a silent baseline on the real corpus
(SC-006); no element flashes >3/s (FR-014).

**Constraints**: content-free announcements only, never transcript text or
unstable hypotheses (constitution V, FR-003); announcement failures must never
block or delay the dictation session (FR-002a); inert/zero-cost when no AT is
listening (FR-007); no keyboard-focus movement from any indicator or
announcement (FR-002, FR-022); no colour-only or sound-only encoding (FR-009,
FR-025); confined-snap compatible (FR-033).

**Scale/Scope**: one new Rust module (`myna-desktop/src/accessibility/`: the
`AccessibilityAnnouncer` trait + `atspi`-backed implementation + fake, wired
into `controller.rs` alongside the existing `Indicator` seam) shared by
`DbusIndicator`/`NotifyIndicator`; one small extension to `indicator::gtk` for
`gtk_accessible_announce`; a small new sound-cue player behind the existing
`Indicator`/controller boundary; one new GJS module pair in
`extensions/myna-shell` (`a11y.js` announcer + updates to `hud.js`/`states.js`);
CLI output changes in `myna-cli`; one checked-in coverage-matrix data file
consumed by both languages; a documented manual acceptance protocol
(`quickstart.md`).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.3.0. This feature spans the same two tiers as feature 004:

- `myna-desktop` and `myna-cli` are shipped Rust system components — all five
  principles apply in full.
- `extensions/myna-shell` is in-compositor GJS UI — **evaluation-harness-tier**
  per the precedent set in feature 004's Constitution Check: exempt from the
  Rust-language rule, strict test-first TDD, and checked-in performance-
  watermark baselines, but still bound by the privacy/offline invariant (V) and
  covered by GJS contract tests + the manual acceptance protocol.

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD | `AccessibilityAnnouncer` trait, coverage-matrix loader/validator, contrast-threshold checker, sound-cue player, and terminal-output changes land test-first behind fakes (Rust). GJS: pure `a11y.js`/coverage-matrix-loading modules get contract tests; actor/bus/animation code is harness-tier (manual acceptance). | PASS (Rust); EXEMPT (extension actor/bus code) |
| II. Integration-Test Readiness | Hermetic tests use a fake `AccessibilityAnnouncer`/fake bus; a new `MYNA_ATSPI_TESTS`-gated suite exercises the real `org.a11y` bus identically on the Workshop VM and hardware, mirroring `MYNA_DBUS_TESTS`. Sound-cue playback integration reuses the existing PipeWire virtual-audio-VM story (`myna-audio`). | PASS (by design) |
| III. Performance Watermarks | Announcement latency (≤500 ms, SC-003) and the "inert when no AT listening" cost (FR-007) are measured Rust watermarks; sound-cue WER delta (SC-006) is measured against the existing real-corpus benchmark harness (`dev/fetch_real_corpus.py` family). Extension fps/latency stays a manual observation (harness-tier exemption, as in feature 004). | GATED — watermark tasks scoped in tasks.md |
| IV. Workshop-Based Dev Environment | New test-only dependency: an AT-SPI bus for `MYNA_ATSPI_TESTS` (e.g. `at-spi2-core`'s bus launcher) added to the Workshop desktop SDK in the same PR as the `atspi` crate. No new *runtime* snap plug expected — snapd's existing `desktop` plug (already declared) grants `org.a11y.Bus` access; confined-build verification (FR-033) confirms this rather than assuming it. | GATED — tracked as a Setup task |
| V. Privacy-First, Offline-First | Announcements, coverage-matrix entries, and sound cues are content-free by construction (state/severity/recovery-action text only, authored once — FR-003, FR-024a); no transcript or raw audio reaches the accessibility bus, a sound file, or a log; no network; audio buffers unchanged (sound-cue playback is output-only, decoupled from the capture buffer). | PASS (by design) |

**Post-Phase-1 re-check**: recorded at the end of this file after research.md/
data-model.md/contracts/quickstart.md are complete.

## Project Structure

### Documentation (this feature)

```text
specs/011-accessible-dictation-ux/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md         # Phase 1 output — manual acceptance protocol (FR-031)
├── contracts/            # Phase 1 output
│   ├── announcer.md       #   AccessibilityAnnouncer seam + AT-SPI wire contract
│   ├── coverage-matrix.md #   state→channel matrix schema + regression gate
│   ├── failure-mapping.md #   plain-language failure taxonomy contract (FR-023–027)
│   └── terminal-output.md #   myna-cli screen-reader-safe output contract (US5)
├── checklists/
│   └── requirements.md  # from /speckit-specify
└── tasks.md             # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
client/
├── myna-audio/                     # UNCHANGED capture side; playback stream added for sound cues
├── myna-orchestrator/               # UNCHANGED seams reused (Indicator, Trigger)
├── myna-desktop/
│   ├── Cargo.toml                   # + atspi dependency; ui-gtk gtk4 feature bump for gtk_accessible_announce
│   ├── src/
│   │   ├── accessibility/           # NEW: the announcer seam (shipped Rust)
│   │   │   ├── mod.rs               #   AccessibilityAnnouncer trait, Announcement{text,severity}, verbosity gate
│   │   │   ├── atspi.rs             #   atspi-backed impl: registers accessible object, emits Announcement,
│   │   │   │                        #     exposes name/description/state for on-demand query (FR-001)
│   │   │   └── fake.rs              #   in-memory fake for hermetic tests
│   │   ├── sound/                   # NEW: sound-cue playback (FR-010/011)
│   │   │   └── mod.rs               #   PipeWire playback stream (reuses myna-audio's client), per-cue toggle
│   │   ├── failure.rs                # NEW: plain-language failure-taxonomy mapping (FR-023/024/024a)
│   │   ├── coverage.rs               # NEW: loads/validates the checked-in state→channel coverage matrix
│   │   ├── indicator/
│   │   │   ├── mod.rs               #   EXTENDED: wire the announcer + coverage lookups alongside Indicator
│   │   │   ├── dbus.rs              #   EXTENDED: on state push, also emit through AccessibilityAnnouncer
│   │   │   ├── gtk.rs               #   EXTENDED (ui-gtk): gtk_accessible_announce() calls
│   │   │   └── notify.rs            #   EXTENDED: recoverable/critical persistence per FR-025/026
│   │   ├── controller.rs             # EXTENDED: verbosity-gated announce-on-transition, coalescing (FR-005)
│   │   └── shortcut/                 # EXTENDED: sticky/slow-keys + autorepeat de-dup verification (US2)
│   └── tests/
│       ├── accessibility.rs          # hermetic: verbosity gating, coalescing, content-free assertion
│       ├── atspi_hw.rs               # NEW, env-gated (MYNA_ATSPI_TESTS): real org.a11y bus round-trip
│       ├── sound_hw.rs               # NEW, env-gated: real PipeWire playback smoke test
│       └── coverage.rs               # hermetic: matrix completeness (every state has ≥1 visual + ≥1 non-visual)
├── myna-cli/
│   └── src/main.rs                   # EXTENDED: colour/emoji-free textual state markers (US5, FR-028/029)
└── Cargo.toml                        # + atspi member dependency

extensions/myna-shell/
├── a11y.js                           # NEW: raw org.a11y.Bus Announcement emission + pure formatting/coalescing
├── hud.js                            # EXTENDED: calls a11y.js on every state/severity transition
├── states.js                         # EXTENDED: adds the content-free announcement text per state
├── coverage-matrix.json               # SHARED (checked-in) data file, also read by the Rust coverage.rs test
└── test/
    ├── a11y.test.js                   # NEW: coalescing/formatting contract tests (pure module)
    └── coverage.test.js               # NEW: matrix completeness, read from the shared JSON

docs/
├── project-plan.md                   # UPDATED: close/annotate T56; note T31/T54/T58/T59/T61/T62 overlap resolution
└── desktop-injection.md              # UPDATED if the announcer seam changes the indicator boundary description
```

**Structure Decision**: The announcer is a new seam (`accessibility::AccessibilityAnnouncer`)
parallel to the existing `Indicator` seam in `myna-desktop`, not folded into
`Indicator` itself — `Indicator` renders visual state; the announcer emits
non-visual events — so a single `controller.rs` transition can drive both
without either seam knowing about the other (FR-006's "identically" requirement
is satisfied by both `DbusIndicator` and `GtkIndicator` calling the same
announcer, not by teaching `Indicator` about AT-SPI). The Shell extension gets
its own `a11y.js` because GJS cannot share Rust code, but both language sides
emit the *same* AT-SPI `Announcement` protocol member, and both read the *same*
checked-in coverage-matrix data file, so FR-024a's "identical wording"
requirement is enforced by a shared artifact rather than by convention. Sound
cues and the failure taxonomy are new, narrowly-scoped modules behind the
existing controller boundary, not new crates — reusing `myna-audio`'s vendored
`pipewire` dependency rather than adding a system sound library (research.md
R2). `GtkIndicator`, `NotifyIndicator`, and `myna-cli` are all extended in
place rather than replaced, per FR-006's non-regression framing and this
feature's decision to cover all three desktop-side surfaces (Shell HUD,
`GtkIndicator`, `NotifyIndicator`) rather than only the shipped default.

## Complexity Tracking

> Only rows that need constitutional justification.

| Violation / Risk | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| **New Rust dependency (`atspi` crate)** | No existing crate in the workspace speaks AT-SPI2; announcements must reach the real accessibility bus, not a GNOME-Shell-only or GTK-only path, because `myna-desktop`'s shipped path (`DbusIndicator`/`NotifyIndicator`) has no GTK widget to call `gtk_accessible_announce()` on. | (a) Depending on GTK just for `gtk_accessible_announce()` — would force the default (non-`ui-gtk`) build to drag in the GTK stack the project deliberately removed from `default` (project-plan T66); (b) shelling out to `spd-say`/speech-dispatcher directly — bypasses the user's actual AT/braille routing and duplicates what AT-SPI already does, and doesn't give programmatic state query (FR-001). |
| **GJS extension emits AT-SPI directly via raw `Gio.DBusConnection`, not a toolkit convenience call** | St/Clutter (unlike GTK 4.14+) has no `announce()`-equivalent (confirmed against `gjs.guide`'s accessibility documentation, which covers roles/relationships/states only). | Polling/mutating `Atk.StateType`/labels on an `St.Widget` does not reliably produce a *proactive* announcement independent of focus — it is the same limitation feature 004 already accepted for the Shell's visual layer; going straight to the AT-SPI protocol primitive is the smallest correct fix and is exactly what GTK does underneath `gtk_accessible_announce()`. |
| **Extension accessibility code (`a11y.js` bus calls, `hud.js` wiring) stays harness-tier, not test-first** | Same GJS/no-nested-compositor constraint already accepted in feature 004's Constitution Check; unchanged here. | Would require a real GNOME Shell session to unit-test the actual bus call — not available headlessly (spec Edge Cases); the pure formatting/coalescing/coverage-matrix logic is extracted and *is* test-first, narrowing the exempt surface to the minimum. |
| **New sound-output code path in `myna-desktop`** (no prior audio-*out* plumbing existed — project-plan T61) | FR-010/011 require optional, WER-safe sound cues; reusing `myna-audio`'s vendored `pipewire` crate for a playback stream is the smallest addition. | `libcanberra` (the freedesktop-standard UI-sound library) was considered and rejected: it is a new C dependency with its own sound-theme/XDG data files, working against this project's explicit, documented size-pruning effort (project-plan T66 removed GTK's icon themes for exactly this reason); PipeWire is already vendored and already the constitution's named primary audio server. |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research.md, data-model.md, contracts/, quickstart.md):

- **I. TDD** — every contract in `contracts/announcer.md`, `coverage-matrix.md`,
  `failure-mapping.md`, and `terminal-output.md` is a row-per-guarantee table
  with an assigned test tier, each landing test-first in Rust (fake announcer,
  fake bus, fixed clock). The GJS side's pure modules (`a11y.js`'s
  `formatAnnouncement`, coverage-matrix loading) get contract tests; the actor/
  live-bus code remains harness-tier, narrowed to exactly the surface research.md
  R6 identifies as headlessly untestable. PASS (Rust) / EXEMPT (extension
  actor/bus code, unchanged scope from feature 004).
- **II. Integration readiness** — the new `MYNA_ATSPI_TESTS` suite mirrors the
  existing `MYNA_DBUS_TESTS`/`MYNA_PIPEWIRE_TESTS` pattern exactly (fake bus
  hermetically, real bus behind an env gate, same on VM and hardware); sound-cue
  playback reuses `myna-audio`'s existing PipeWire integration story. PASS.
- **III. Performance Watermarks** — SC-003 (≤500 ms announce latency), FR-007
  (inert when unobserved), and SC-006 (WER delta ≤0.5pp) are each a named,
  measurable watermark with a declared tolerance, scoped as tasks in tasks.md.
  GATED — tracked, not yet measured (expected: measured in the corresponding
  implementation task, per Principle III's "MUST ship with measurements").
- **IV. Workshop** — one new Workshop dependency identified (an AT-SPI bus for
  `MYNA_ATSPI_TESTS`); scoped as a Setup-phase task. No new *runtime* snap plug
  expected (existing `desktop` plug), confirmed rather than assumed by
  quickstart.md Scenario 7. GATED — tracked.
- **V. Privacy** — every new artifact (announcements, coverage matrix, sound
  cues, failure presentations) is content-free and authored-once by
  construction; no new network dependency; no capture-path change. PASS.

No new violations were introduced by the Phase 1 design; the Complexity
Tracking table above is unchanged.
