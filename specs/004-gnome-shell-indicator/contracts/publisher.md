# Contract: myna-desktop D-Bus publisher (Rust)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; architecture revision: 2026-08-26)

The shipped Rust half: a `DbusIndicator` (`Indicator` backend) + a `DbusTrigger`
(`Trigger` backend) + a `dbus` module serving `org.myna.Dictation`
(`contracts/dbus-interface.md`). All guarantees encoded as tests before code
(constitution I). Boundary to the bus is a small `Bus` seam with a fake
implementation for hermetic tests (R11). **(2026-08-26)** adds the indicator-
surface **launcher policy** (§Launcher policy): `myna-desktop` watches the
`org.myna.Shell` presence name and suppresses its notification fallback while
the extension host is up; and the old experimental `ui-gtk`/`GtkIndicator`
overlay is **removed** (superseded by the `myna-hud` renderer application —
spec FR-023; its guarantees were never listed here and its files are deleted).

## DbusIndicator (implements `indicator::Indicator`)

| # | Guarantee | Test tier |
|---|---|---|
| P1 | `set_state(s)` maps `IndicatorState` → the E1 `State` string and updates the `State` property (pushed via `PropertiesChanged`). | hermetic (fake bus) |
| P2 | The `Loading`→`Ready` split surfaces as `loading` then `recording` (R4/C5): the indicator tracks whether `Ready` has been seen this session. | hermetic |
| P3 | `hide()` publishes `idle`, zeroes `AudioRms`/`AudioPeak`, and clears `ErrorMessage`. | hermetic |
| P4 | Error state carries the existing content-free message via the `ErrorMessage` property; never transcript (C3). | hermetic |
| P5 | Is a drop-in `Indicator`: the controller wiring is unchanged; `DbusIndicator` composes with `NotifyIndicator` as fallback (both can run; D-Bus preferred). | hermetic + compile |
| P16 | **(2026-07-30)** `map_state` publishes `notice` when `IndicatorState::Error{recoverable: true, ..}` and `error` when `recoverable: false` — the two are mutually exclusive per call (C10). | hermetic |

## Completion severity (2026-07-30)

| # | Guarantee | Test tier |
|---|---|---|
| P17 | `completion_indicator_state(transcript)` returns `IndicatorState::Error{message: "No speech detected", recoverable: true}` for an empty/blank transcript, and `IndicatorState::Hidden` otherwise. | hermetic (`controller.rs`) |
| P18 | Both the live per-event path (`event_to_indicator`'s `Done(_)` arm) and the finalize-block safety net (`SessionOutcome::Completed{transcript}`) call `completion_indicator_state` with the same transcript and therefore always agree (C11) — asserted directly, not just by inspection. | hermetic (`controller.rs`) |
| P19 | The `IndicatorState::Error` field addition (`recoverable: bool`) does not change `gtk::GtkIndicator` or `notify::NotifyIndicator` rendering for any existing `Error` case — both continue to render every error identically regardless of the new field's value. | hermetic (`indicator/gtk.rs`, `indicator/notify.rs`) |

## Level pump

| # | Guarantee | Test tier |
|---|---|---|
| P6 | Subscribes to `CaptureSource::stats()` (`watch::Receiver<AudioStats>`) and publishes `AudioRms`/`AudioPeak` at ~15–20 Hz while recording (C4). | hermetic (fed AudioStats) |
| P7 | Publishes `0.0` levels at idle / session end; never publishes samples or content (constitution V). | hermetic |
| P8 | Adds no capture-path regression: it only *reads* the existing `watch` and emits — no new work on the audio thread (III). | watermark (reuses 002/003 baseline) |

## DbusTrigger (implements `orchestrator::Trigger`, sibling of `ControlTrigger`)

| # | Guarantee | Test tier |
|---|---|---|
| P9 | `Toggle` alternates `Press`/`Release` edges; `Start` yields `Press` when idle, `Stop` yields `Release` when active (C6). | hermetic |
| P10 | Duplicate/rapid `Start`/`Toggle` do not start two sessions (dedup — one Press until a Release), matching `ControlTrigger`'s alternation. | hermetic |
| P11 | `Start` returns `(false, reason)` when the session cannot start; the reason is content-free (C7). | hermetic + gated |
| P12 | Trigger exhaustion (name released / daemon shutdown) ends the edge stream cleanly (`None`), like the other triggers. | hermetic |

## Serving / lifecycle

| # | Guarantee | Test tier |
|---|---|---|
| P13 | `myna-desktop --dbus` requests `org.myna.Dictation` on the session bus and serves `/org/myna/Dictation`; a real client sees it (C1/C9). | env-gated `MYNA_DBUS_TESTS=1` |
| P14 | On shutdown the name is released so watchers see name-vanished (C9). | env-gated |
| P15 | The `--dbus` mode falls back to `NotifyIndicator` when the session bus is unavailable (never a hard failure of dictation). | hermetic (bus-open error path) |

## Launcher policy (2026-08-26, R24; C13)

`myna-desktop` selects the indicator surface from the presence name instead of
rendering its own:

| # | Guarantee | Test tier |
|---|---|---|
| P20 | While `org.myna.Shell` has an owner, the fallback notification indicator is suppressed (no duplicate indicator beside the hosted renderer); dictation behavior is otherwise unchanged. | hermetic (fake presence seam) |
| P21 | When `org.myna.Shell` vanishes (extension disabled/removed/Shell crash), the fallback notification indicator is restored. | hermetic (fake presence seam) |
| P22 | Presence watching never blocks or fails dictation: a bus error degrades to the fallback surface, never an abort. | hermetic (bus-open error path) |
| P23 | The non-GNOME spawn path (launch `myna-hud` standalone where a focus-safe overlay backend exists) is **contract only**: the policy hook exists behind a seam, no backend ships this pass (spec Out of Scope). | seam + unit test of the policy function |

## Removals (2026-08-26)

- `indicator::gtk::GtkIndicator` and the `ui-gtk` cargo feature are deleted
  (superseded by the `myna-hud` renderer application; spec FR-023). The
  `Indicator` trait and its `DbusIndicator`/`NotifyIndicator` backends are
  unchanged; `--overlay` mode is removed from the binary's CLI.
- No new wire members accompany the architecture revision — the presence name
  is member-less (C12/C13) and the dictation interface is untouched.

## Non-goals (publisher)

- No transcript ever crosses the bus (only state + level + reason).
- No settings/config surface (no model/mic/language over D-Bus — Out of Scope).
- No auto-start / D-Bus activation of the daemon (lifecycle owned elsewhere).
- **(2026-07-30)** No true wire-level error disposition/taxonomy — the
  `recoverable`/`notice` classification is an interim, client-inferred stopgap
  (T31/T62 remain the owners of that future work).

