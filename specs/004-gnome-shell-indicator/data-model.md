# Phase 1 Data Model: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; architecture revision: 2026-08-26)

The entities crossing the indicator↔`myna-desktop` seam and the indicator's own
transient state. Nothing here is persisted; audio is never represented (only
derived level). Cross-refs to spec Key Entities and the D-Bus contract
(`contracts/dbus-interface.md`). **(2026-08-26)**: the indicator surface is now
the window of the renderer application (`myna-hud`); the entities below move
across that split — dictation-facing entities (E1–E3, E5) are consumed by the
renderer application (previously the extension); the extension's own transient
state is the hosted-window bookkeeping (E7). `com.canonical.Myna.Shell`
presence (E6) is removed — fallback suppression now uses the `RegisterClient`
client set.

## E1 — DictationState (the wire state string)

The single source of truth for the HUD pill's treatment. String enum on the
`com.canonical.Myna.Dictation.State` property (updates pushed via `PropertiesChanged`).

| Value | Meaning | Derived in myna-desktop from |
|---|---|---|
| `idle` | No session; HUD pill hidden. | `IndicatorState::Hidden` |
| `loading` | Cold model load in progress (distinct treatment, FR-006). | `OrchestratorEvent::Loading` seen, `Ready` not yet |
| `recording` | Capturing / listening. | `IndicatorState::Recording` (post-`Ready`) |
| `transcribing` | Inference decoding. | `IndicatorState::Transcribing` |
| `finalizing` | Release seen; awaiting terminal transcript. | `IndicatorState::Finalizing` |
| `notice` | **(2026-07-30)** Session completed with an empty/blank transcript — a recoverable, non-blocking issue (e.g. "no speech detected"). | `IndicatorState::Error{recoverable: true, ..}`, from `completion_indicator_state()` on an empty `SessionOutcome::Completed{transcript}` |
| `error` | Failure / secure-field refusal — a critical, persistent issue. | `IndicatorState::Error{recoverable: false, ..}` |

Rules:
- Additive/forward-compatible: an **unknown** value (including `notice` seen by
  an unpatched consumer build) MUST map to a neutral "active" visual intent on
  the consumer (renderer) side, never an exception (spec FR-008, R3).
- Privacy: the value is a state label only — never transcript text (constitution
  V; the `Error`/`Notice` message is a user-facing reason string, still
  content-free). The empty-transcript check that produces `notice` happens
  server-side in `myna-desktop`; only the boolean outcome (as a `State` value)
  crosses the bus.
- Legal transitions mirror the controller's audited `DictationState` model
  (`controller.rs`): idle→loading|recording, loading→recording|error|idle,
  recording→transcribing|finalizing|error|idle, transcribing→recording|finalizing|
  error|idle, finalizing→idle|error|notice, notice→idle, error→idle. The
  extension does **not** enforce legality (it renders whatever arrives,
  degrading unknowns); the publisher is the authority.
- `notice` and `error` are mutually exclusive per transition (a single
  `IndicatorState::Error{recoverable, ..}` maps to exactly one of the two wire
  values, never both) — see E1a (Severity) and R13.

## E1a — Severity (recoverable vs. critical, 2026-07-30)

Realized as the choice between the `notice` and `error` wire state values
themselves (E1) — not a separate D-Bus property. Backed in Rust by
`IndicatorState::Error { message: String, recoverable: bool }`.

| Severity | Wire `State` | UX treatment | Auto-dismiss? | Wave ribbon (2026-07-30, R17a) |
|---|---|---|---|---|
| `recoverable` | `notice` | Non-blocking notice; a new session may start while it's showing | Yes — ~3.5 s hold, restarts in full if a new recoverable notice arrives while one is showing (R15) | **Visible**, tinted amber, audio-reactivity paused (gentle idle pulse) — not hidden |
| `critical` | `error` | Persistent notice, no control of any kind **(2026-08-26)** | No — remains until the **client** publishes a different state; a new critical error while one is showing replaces the reason in place and still never auto-clears (R15) | Hidden (the pill's icon/border/message carry the state instead) |

This is an **interim, client-inferred classification** (spec Assumptions;
research R13) pending T31/T62's wire-level error taxonomy — not itself that
taxonomy.

## E2 — AudioLevel (the wave-ribbon input)

| Field | Type / range | Source | Notes |
|---|---|---|---|
| `AudioRms` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.rms` | drives the wave ribbon's envelope/strand generation (dominant input, stable) |
| `AudioPeak` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.peak` | blended in at reduced weight so transients/consonants are visible without pinning the meter |

Rules:
- Published only while a session is active; `0.0` when `idle` (spec FR-011).
- Updated at ~15–20 Hz (R5); the extension applies **stale-decay** to floor if no
  update arrives within ~300 ms (spec FR-011 / SC-004), based on **arrival
  time**, not value — a repeated identical RMS/peak still counts as fresh
  (R16a; a 2026-07-30 manual-test regression found the extension had been
  dropping "unchanged" updates and treating a steady voice as stale).
- **(2026-07-30, R17 — wave-ribbon redesign)**: RMS+peak are combined through
  the same hardware-calibrated dBFS scale R16a established
  (`ribbon.js`'s envelope smoothing, reusing `vumeter.js`'s `boostLevel`/
  stale-decay unchanged), then used to generate ~3 strands × 12–20 control
  points each (small per-strand phase/delay/amplitude offsets off the *same*
  envelope value — never independent per-strand state) painted by the shared
  `ribbon-paint.js`. Replaces R16's left-to-right segmented-bar rendering of
  the same underlying intensity value; the envelope math itself (R16a's
  calibration + stale-decay-by-arrival-time) is unchanged and reused verbatim.
- **(2026-07-30, R17a — "fabric in gentle airflow" refinement)**: a SECOND
  smoothing stage sits between the calibrated instantaneous envelope above
  and the wave shape: `ribbon.js`'s `applyEnvelopeSmoothing`, a one-pole
  low-pass with a ~300 ms time constant (`SMOOTHING_TAU_MS`, 250-400 ms
  design range), maintained as caller-owned state across repaint frames
  (same pattern as phase/phaseStartedAt) so `ribbon.js` itself stays a pure
  function of its inputs. This is what keeps the ribbon reading as a
  smoothed, controlled interpretation of loudness rather than a literal,
  oscilloscope-like reproduction of the envelope tick-by-tick.
- Carries energy only — never samples, never content (constitution V, R5).

## E2a — AccentColorPreference (renderer-side, sourced from the desktop, 2026-07-30; mechanism amended 2026-08-26)

| Field | Type / Source | Notes |
|---|---|---|
| themeAccent | `RGBA \| null` | the **theme's own accent**, used when the style manager has no accent property (libadwaita < 1.6) or a stylesheet overrides the accent independently: a widget styled `color: @accent_bg_color`, read back with `gtk_widget_get_color()` (R26). The named colour exists since libadwaita 1.0, so no version probing is needed, and Yaru's tints and variants — including `wartybrown`, which has no upstream enum member — resolve by construction | `null` only when no styled widget is rooted yet |
| resolvedColor | derived palette (main / highlight / darker-complement / translucent secondary) | main color = `AdwStyleManager:accent-color-rgba`, falling back to `themeAccent`, then to a fixed Ubuntu-orange (`#E95420`). The `accent-color` **name table is gone** (2026-08-26): it mapped a settings name onto a colour ourselves, which the theme already does — and does better, covering Yaru variants and `wartybrown`, which has no upstream enum member. The `accent-color`/`gtk-theme` keys are still *watched*, purely as change triggers. Derived tones are pure, tested logic. The darker-complement tone is a computed colour complement of the main colour, **except when the main colour is orange** (`#e95420` or `#ed5b00`), **where it is a fixed aubergine tone** (matching the reference design decision) rather than a generic computed complement — keyed on the colour, since the theme path has no settings name |

Rules:
- **No "did the user actively choose" test (amended 2026-08-26).** The
  earlier rule treated an untouched default as "not chosen" and forced
  Ubuntu orange, using the GSettings *user value*. Its premise was false —
  `ubuntu-settings` ships a gschema override making the effective default
  `'orange'`, so the resolved value was already correct — and the gate
  misfired on Yaru accent variants, re-tinting a visibly olive desktop back
  to orange. The theme reports what the desktop is actually using; that is
  what the ribbon uses.
- Read live, but **never from a raw GSettings handler**: the accent is a
  *computed* CSS colour, and there it is still the previous value, since
  libadwaita listens to the same key with no defined ordering between it and
  us. The primary trigger is libadwaita's own
  `AdwStyleManager::notify::accent-color-rgba`, which by construction is
  emitted *after* the provider defining `@accent_bg_color` is reloaded, so
  the accent is read immediately there. Triggers without that guarantee
  (`changed::accent-color`, `changed::gtk-theme`, `notify::gtk-theme-name`,
  the ribbon being mapped) schedule a **one-shot** re-read on the next frame.
  Neither path costs anything per repaint.
- Sourced entirely from the desktop environment, not from `myna-desktop` or
  the D-Bus contract — no wire change (data-model E2/dbus-interface.md
  unaffected).
- Safe where accent colors are unsupported: degrades to the same
  Ubuntu-orange fallback, never an exception (R18/R26).

## E2b — MotionPreference (renderer-side, sourced from the desktop, 2026-07-30; mechanism amended 2026-08-26)

| Field | Type / Source | Notes |
|---|---|---|
| reducedMotion | `boolean` | primary source: GTK's `GtkSettings:gtk-interface-reduced-motion` (enum `GtkReducedMotion`: `no-preference`/`reduce`; GTK ≥ 4.22), which GDK populates from the settings portal (`org.freedesktop.appearance reduced-motion`) with a safe default when absent. Fallback when the GTK property doesn't exist (older GTK): the old `org.gnome.desktop.interface` `enable-animations` key (inverted), schema/key-existence guarded as before (R19/R26) | when `true`, the ribbon renders a static level line / gently-scaling mic indicator instead of the flowing wave (FR-022a) |

Rules:
- **Never read `org.gnome.desktop.a11y.interface`'s `reduced-motion` key
  directly** — it is a *new* key (gsettings-desktop-schemas, 2026 cycle) and
  is absent on older systems; an unguarded `Gio.Settings`/`gio::Settings`
  construction or read against a missing schema/key aborts the process.
  Going through `GtkSettings` (or, at minimum, the same schema+key existence
  guard R18 established) is mandatory (crash-on-start risk, flagged 2026-08-26).
- Read live (`notify::gtk-interface-reduced-motion` on the settings object;
  libadwaita's style manager tracks the same property), same pattern as E2a.
- Drives only the *rendering* choice (static vs. flowing) — the underlying
  level/state inputs (E2) are unaffected either way (FR-022a: "still conveys
  state and level").

## E3 — ErrorReason (optional)

A short, user-facing reason string carried by the `ErrorMessage` property
when `State == error` **or `State == notice`** (2026-07-30: broadened —
previously `error`-only) — e.g. "no text field is focused", "refusing to type
into a password field", "inference backend unavailable" (critical/`error`), or
"No speech detected" (recoverable/`notice`). Empty when neither. Content-free
(never transcript). Sourced from the controller's existing
`IndicatorState::Error{message, ..}` / `abort_before_capture` messages, and
(2026-07-30) from `completion_indicator_state()`'s fixed reason for the
empty-transcript case.

## E4 — IndicatorSurface (renderer-side, transient; rewritten 2026-08-26)

The renderer application's in-memory view; not on the wire. (Previously the
extension's actor state; the same fields move with the drawing — plus the
window/input-region state the windowed surface adds.)

| Aspect | Description |
|---|---|
| current state | last `DictationState` received (default `idle`) |
| current level | last `AudioLevel` + a timestamp (for stale-decay) |
| HUD pill window | the application's single borderless toplevel; mapped/visible only while state ≠ `idle` (the hosted overlay window of spec Key Entities); contains the pill layout, status label, mic/mic-slash icon, and the GLArea ribbon. No interactive control **(2026-08-26)** |
| ribbon rendering | the wave ribbon renders via the GPU shader path only (R23) in a GLArea driven by the frame clock, gated on mapped + not reduced-motion |
| input region | empty (fully click-through) in **every** state, with no exception **(2026-08-26, R22)**; re-applied on map |
| held notice slot | one severity-scoped slot (reason + optional dismiss-timer handle) implementing the replace-in-place/restart-timer rules (R15) |
| ~~dismiss control~~ | **Removed 2026-08-26.** The HUD takes no pointer input; a critical error is cleared by the client publishing a different state (FR-007b as amended, FR-025) |
| ribbon severity tint | `descriptor.severity` passed straight through to the ribbon as its `severityTint` (`null \| 'recoverable' \| 'critical'`); the ribbon stays visible/amber/paused-pulsing for `'recoverable'`, hidden for `'critical'` (FR-010e) |
| a11y | the window and its children expose accessible labels/roles via the toolkit's accessibility bridge, updated per state (R10's parity requirement, now native to the app) |
| accent color | current `AccentColorPreference` (E2a), re-read live; colors the ribbon strands |
| motion preference | current `MotionPreference` (E2b), re-read live; selects flowing vs. static ribbon rendering |
| panel button | optional `PanelMenu.Button` remains an extension-side future affordance (R8, US4) — unaffected, not part of the renderer application |

Rules:
- No mapped window while `idle` (push-to-talk, spec FR-002); window content +
  timers + transitions torn down on `idle`, on name-vanished, and on app exit
  (spec FR-021 — no leaks).
- Nothing rendered, logged, or stored carries transcript content (constitution V).
- A second `notice`/`error` of the same severity while one is showing updates
  the held slot in place; it never creates a second concurrent
  notice (R15 — no stacking or queuing).

## E5 — Availability (renderer-side, transient; moved 2026-08-26)

Boolean derived from name-watching `com.canonical.Myna.Dictation`
(name-appeared → available; name-vanished → unavailable). Drives dormancy: while
unavailable, the renderer shows no window and surfaces no error (spec FR-018,
US1-5), and clears any HUD pill to idle on transition to unavailable (crash
mid-session edge case). Previously extension-side; semantics unchanged.

## E6 — removed: `com.canonical.Myna.Shell` presence name (was R24/FR-017a, 2026-08-26) is no longer exposed. Fallback suppression uses the `RegisterClient` client set on `com.canonical.Myna.Dictation` instead.

## E7 — HostedWindow (extension-side, transient; new 2026-08-26)

The extension host's bookkeeping for the renderer application's window; not on
the wire.

| Aspect | Description |
|---|---|
| wayland client | the `Meta.WaylandClient` handle created at `enable()` by spawning `myna-hud` (R21) |
| subprocess | the spawned process handle (for exit watching / forced termination) |
| adopted window | the `Meta.Window` owned by the client (identified via `owns_window()`; fallback `get_sandboxed_app_id()`/PID if the snap path requires it, R27) — re-typed DOCK, hidden from window lists, kept above, all workspaces |
| position | bottom-center of the primary monitor's work area (R21's placement math, pure + unit-tested); recomputed on monitors/workarea/size changes with anti-feedback-loop guards |
| supervision | respawn state: last exit time, bounded backoff, restart budget (FR-026) |
| adopted state | whether adoption completed (window exists + typed + positioned); the indicator is not considered up until then |

Rules:
- One renderer process per enabled extension; terminated on `disable()` and on
  `unmanaged` teardown paths (no orphans; spec FR-021).
- The host never reads dictation state or levels — it has no
  `com.canonical.Myna.Dictation` proxy at all; its only bus surface is E6.

## State → visual-intent mapping (pure; contract-tested)

**(2026-08-26)** Lives in the renderer application (Rust, `myna-hud`'s pure
state module — ported 1:1 from `extensions/myna-shell/states.js`) and is
unit-testable without a display (R11's split, relocated). Maps
`DictationState` → a visual-intent record consumed by the HUD:
the descriptor shape is `{key, statusText, severity, hidden}`, where `severity`
is `'recoverable' | 'critical' | null`. Status strings are translatable under
the `myna` gettext domain (R25).

| State | icon | severity | a11y label |
|---|---|---|---|
| `idle` | — (hidden) | `null` | (actor absent) |
| `loading` | mic (filled) | `null` | "Dictation: loading model" |
| `recording` | mic (filled) | `null` | "Dictation: listening" |
| `transcribing` | mic (filled) | `null` | "Dictation: transcribing" |
| `finalizing` | mic (filled) | `null` | "Dictation: finishing" |
| `notice` | mic (filled — the mic itself isn't the fault) | `'recoverable'` | "Dictation: no speech detected" |
| `error` | mic-with-slash | `'critical'` | "Dictation: error — <reason>" |
| *(unknown)* | mic (filled) | `null` | "Dictation: active" |

The visual-intent record is CSS-class + icon-choice + label — no transcript,
tunable in `stylesheet.css` without touching the mapping.

