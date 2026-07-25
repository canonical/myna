# Quickstart / Validation: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

Runnable validation that the focus-safe goop indicator works end-to-end. See
`contracts/` for the guarantees each step proves and `plan.md` for structure.

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
cargo test -p myna-desktop dbus_indicator     # C2–C7, P1–P12 over the fake bus
```

**Expected**: state→`State`-string mapping (incl. the loading/recording split),
content-free payloads, level throttling, and `DbusTrigger` dedup all pass. These
are written red-first (constitution I).

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
```

**Expected**: `states.js` maps all known states + a neutral unknown (X1–X4),
`vumeter.js` is monotonic/clamped and decays on stale (X5), no output carries
content (X6), and the stub-proxy lifecycle (dormant / appeared / vanished /
disable / re-enable) holds (X7–X10).

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
myna-desktop --install-shortcut                               # once: binds Super+D (feature 003)
# focus a text field (GNOME Text Editor), then:
#   tap Super+D  → goop appears (loading glow if cold, then listening ripple)
#   speak        → glow/VU tracks your voice
#   tap Super+D  → finalizing flash, text injected via IBus, goop clears
```

**Expected / assert**:
- Goop appears within ~100–200 ms of start and clears after stop (X12, SC-003).
- **Focus is never stolen**: while the goop is visible, keep typing in the field —
  characters land there (X11, SC-001). *This is the whole point of the feature.*
- States are distinct: a cold model load shows the **loading** glow, not the
  listening ripple (X13, FR-006).
- The VU glow rises when you speak and eases to floor on silence (X14, SC-004).
- The injected transcript matches what you said (feature 003 unchanged; the
  extension added no injection).
- No transcript text ever appears in the goop or in logs (X6, SC-005).

## 6. Panel toggle (optional, P3)

With the panel button enabled: click it → a session starts (goop appears); click
again → it stops and commits, identical to the hotkey (X16, SC-010). With
`myna-desktop --dbus` not running, the button is dimmed (unavailable).

## 7. Robustness spot-checks (edge cases)

```sh
# daemon crash mid-session:
kill %1        # while a session is active → goop clears to idle (X8), no error spew
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
  never stolen by the goop, with all states legible.
- `docs/desktop-injection.md` §2 updated to record this extension as the GNOME
  focus-safe overlay answer (NotifyIndicator remains the fallback).
