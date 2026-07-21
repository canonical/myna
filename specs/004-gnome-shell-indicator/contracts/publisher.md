# Contract: myna-desktop D-Bus publisher (Rust)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

The shipped Rust half: a `DbusIndicator` (`Indicator` backend) + a `DbusTrigger`
(`Trigger` backend) + a `dbus` module serving `org.myna.Dictation`
(`contracts/dbus-interface.md`). All guarantees encoded as tests before code
(constitution I). Boundary to the bus is a small `Bus` seam with a fake
implementation for hermetic tests (R11).

## DbusIndicator (implements `indicator::Indicator`)

| # | Guarantee | Test tier |
|---|---|---|
| P1 | `set_state(s)` maps `IndicatorState` → the E1 `State` string and emits `StateChanged` + updates the `State` property. | hermetic (fake bus) |
| P2 | The `Loading`→`Ready` split surfaces as `loading` then `recording` (R4/C5): the indicator tracks whether `Ready` has been seen this session. | hermetic |
| P3 | `hide()` publishes `idle`, zeroes `AudioRms`/`AudioPeak`, and clears `ErrorMessage`. | hermetic |
| P4 | Error state carries the existing content-free message via `ErrorMessage` + `StateChanged` arg; never transcript (C3). | hermetic |
| P5 | Is a drop-in `Indicator`: the controller wiring is unchanged; `DbusIndicator` composes with `NotifyIndicator` as fallback (both can run; D-Bus preferred). | hermetic + compile |

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

## Non-goals (publisher)

- No transcript ever crosses the bus (only state + level + reason).
- No settings/config surface (no model/mic/language over D-Bus — Out of Scope).
- No auto-start / D-Bus activation of the daemon (lifecycle owned elsewhere).
