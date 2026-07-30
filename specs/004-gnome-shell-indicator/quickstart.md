# Quickstart / Validation: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30)

Runnable validation that the focus-safe HUD pill indicator works end-to-end. See
`contracts/` for the guarantees each step proves and `plan.md` for structure.
**(2026-07-30)**: the "goop" ribbon is replaced by a bottom-center HUD pill
(`hud.js`); steps below are updated to also exercise the recoverable-notice and
critical-error severities (C10/C11, X19–X22).


## Prerequisites

- Ubuntu 26.04+ on **Wayland**, GNOME Shell **50 or 51** (`gnome-shell --version`).
  Dev-loop note (2026-07-21, verified on Shell 50.2): mutter **removed the
  nested backend** — there is no `gnome-shell --nested`, plain `--wayland` does
  not nest, and the `--devkit` viewer (`mutter-devkit`) is not shipped by
  Ubuntu's mutter packages. On Wayland the only code-reload is a Shell
  restart (log out/in); iterate on pure logic via the GJS contract tests
  (step 3) and batch Shell-side checks into as few reloads as possible.
- The Workshop dev env (`.workshop/myna.yaml`) with the desktop SDK (D-Bus, GJS,
  gnome-shell) — see the foundational task; or a GNOME session on hardware.
- A running inference backend, e.g. `myna-server --adapter whisper --socket /tmp/myna.sock`.
- IBus running (feature 003) for the actual text injection.

## 1. Hermetic publisher tests (no bus, no Shell) — Rust, TDD

```sh
cd client
cargo test -p myna-desktop dbus_indicator     # C2–C7, C10–C11, P1–P12, P16–P19 over the fake bus
cargo test -p myna-desktop controller         # completion_indicator_state, empty-transcript → notice
```

**Expected**: state→`State`-string mapping (incl. the loading/recording split
and the new notice/error severity split), content-free payloads, level
throttling, and `DbusTrigger` dedup all pass; `completion_indicator_state`
returns `notice` for an empty/blank transcript and `idle` otherwise, agreeing
across both call sites (C11); `gtk.rs`/`notify.rs` render every error
identically regardless of the new `recoverable` field (P19). These are written
red-first (constitution I).

## 2. Env-gated publisher integration (real session bus) — VM or hardware

```sh
cd client
MYNA_DBUS_TESTS=1 dbus-run-session -- cargo test -p myna-desktop dbus_hw
```

**Expected**: `myna-desktop` (test harness) claims `org.myna.Dictation`, a `zbus`
client observes `PropertiesChanged` + reads `State`/`AudioRms`/`AudioPeak`, and
name-appeared/vanished fire on start/stop (C1, C9, P13–P14). Runs identically on
the desktop VM and hardware (constitution II).

## 3. GJS contract tests (pure mapping + lifecycle) — no Shell

```sh
cd extensions/myna-shell
gjs -m test/states.test.js
gjs -m test/hud.test.js
```

**Expected**: `states.js` maps all known states (including `notice`/`error`
severities) + a neutral unknown (X1–X4, X19), `vumeter.js` is monotonic/clamped
and decays on stale (X5), no output carries content (X6), and the stub-proxy
lifecycle (dormant / appeared / vanished / disable / re-enable) holds (X7–X10).
`hud.test.js` asserts the replace-in-place/restart-timer behavior for repeated
notices/errors and that the dismiss control's reactive-but-non-focusable
property holds (X20).

## 4. Install & enable the extension — GNOME session

```sh
UUID=myna-shell@myna.dev
mkdir -p ~/.local/share/gnome-shell/extensions/$UUID
cp -r extensions/myna-shell/* ~/.local/share/gnome-shell/extensions/$UUID/
# Wayland: log out/in to reload the Shell (Alt+F2 r is X11-only).
gnome-extensions enable $UUID
gnome-extensions info $UUID        # → State: ENABLED
```

**Expected**: extension enabled, dormant (no overlay) because `myna-desktop --dbus`
is not yet running (X7).

## 5. End-to-end spoken run (the on-hardware acceptance)

```sh
myna-desktop --dbus --socket /tmp/myna.sock --language en &   # serves org.myna.Dictation
myna-desktop --install-shortcut '<Super>t>'                              # once: binds a shortcut (feature 003)
# focus a text field (GNOME Text Editor), then:
#   tap the shortcut  → HUD pill appears bottom-center (loading treatment if cold, then listening)
#   speak        → VU meter lights green→yellow→red with your voice
#   tap the shortcut  → finalizing treatment, text injected via IBus, pill clears
```

**Expected / assert**:
- HUD pill appears within ~100–200 ms of start and clears after stop (X12, SC-003).
- **Focus is never stolen**: while the pill is visible, keep typing in the field —
  characters land there (X11, SC-001). *This is the whole point of the feature.*
- The pill renders bottom-center of the primary monitor, matching GNOME's own
  volume/brightness OSD position (X21, FR-004).
- States are distinct: a cold model load shows the **loading** treatment, not the
  listening treatment (X13, FR-006).
- The VU meter lights more segments as you speak at normal volume (calibrated
  to real speech, not a raw linear gain — R16a) and eases to floor on silence
  (X14, SC-004).
- The injected transcript matches what you said (feature 003 unchanged; the
  extension added no injection).
- No transcript text ever appears in the pill or in logs (X6, SC-005).

## 5a. Recoverable-issue walkthrough (2026-07-30, US2a)

```sh
# tap the shortcut, then tap again immediately without speaking:
#   tap → tap (no speech in between)
```

**Expected / assert**:
- A non-blocking notice appears (e.g. "No speech detected") — mic icon stays
  filled (not slashed), since the microphone itself isn't at fault (X19).
- The notice clears on its own after ~3.5 s with no user action (X13, C10).
- Starting a new session immediately (before the notice clears) proceeds
  normally and is not blocked or delayed by the still-visible notice.
- If a second "no speech" happens while the first notice is still showing, the
  notice updates in place and the auto-dismiss timer restarts in full — it does
  not clear early on the original schedule (X20, R15).

## 5b. Critical-error walkthrough (2026-07-30, US2a)

```sh
# simulate a hard failure, e.g. unplug/disable the microphone, or stop the
# backend mid-session, then tap the shortcut.
```

**Expected / assert**:
- A persistent notice appears with a clear, content-free reason and a mic-with-
  slash icon (X19) and a visible dismiss (×) control.
- The notice does **not** clear on its own — verify by waiting well past the
  recoverable-notice's ~3.5 s window.
- Clicking the × clears the notice immediately (X22).
- **Focus is never stolen** by clicking the ×: keep a text field focused,
  click the dismiss control, and confirm typing still lands in the field (X11,
  SC-001) — the control is pointer-reactive but never keyboard-focusable.
- If a second critical error arrives before the first is dismissed, the notice
  updates in place to the new reason and still requires an explicit dismiss
  (X20, R15).

## 6. Panel toggle (optional, P3)

With the panel button enabled: click it → a session starts (HUD pill appears); click
again → it stops and commits, identical to the hotkey (X16, SC-010). With
`myna-desktop --dbus` not running, the button is dimmed (unavailable).

## 7. Robustness spot-checks (edge cases)

```sh
# daemon crash mid-session:
kill %1        # while a session is active → pill clears to idle (X8), no error spew
# disable cleanliness:
gnome-extensions disable $UUID     # actors/timers gone; re-enable re-inits (X9/X10)
```

## 8. Watermarks (publisher) — constitution III

```sh
cd client
cargo test -p myna-desktop --test watermarks    # + the new dbus level-pump cadence check
```

**Expected**: state-push→property-update latency and level-pump cadence within
declared tolerances; no capture-path regression vs the feature-002/003 baselines.
Extension fps is a manual observation in step 5 (harness-tier exemption).

## Done when

- Steps 1–3 green (hermetic + gated publisher, GJS contract).
- Step 5 passes on hardware: spoken text lands in the focused app **and** focus is
  never stolen by the HUD pill, with all states legible.
- Steps 5a/5b pass: the recoverable notice auto-dismisses and never blocks a new
  session; the critical error persists until dismissed and its × control never
  steals focus.
- `docs/desktop-injection.md` §2 updated to record this extension as the GNOME
  focus-safe overlay answer (NotifyIndicator remains the fallback).
