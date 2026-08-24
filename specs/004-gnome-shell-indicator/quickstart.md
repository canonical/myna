# Quickstart / Validation: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30)

Runnable validation that the focus-safe HUD pill indicator works end-to-end. See
`contracts/` for the guarantees each step proves and `plan.md` for structure.
**(2026-07-30)**: the "goop" ribbon is replaced by a bottom-center HUD pill
(`hud.js`); steps below are updated to also exercise the recoverable-notice and
critical-error severities (C10/C11, X19–X22). **(2026-07-30, wave-ribbon)**:
the segmented bar meter is further replaced by a flowing, accent-colored wave
ribbon (X14, X24–X28); a new optional step 3a covers the standalone `dev-lab`
tuning tool.


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
gjs -m test/ribbon.test.js
gjs -m test/accent.test.js
```

**Expected**: `states.js` maps all known states (including `notice`/`error`
severities) + a neutral unknown (X1–X4, X19), the envelope smoothing in
`ribbon.js` (reusing `vumeter.js`'s calibrated math unchanged, plus the
2026-07-30 refinement's second ~300ms smoothing stage) is monotonic/clamped
and decays on stale (X5), layered strand/control-point generation and the 5
lifecycle-phase timing functions are deterministic, including the `morph`→
travelling-dots and `complete`→convergence-point transitions and the
recoverable severity's amber tint (X24, X30, X31), no output carries content
(X6), and the stub-proxy lifecycle (dormant / appeared / vanished / disable
/ re-enable) holds (X7–X10). `hud.test.js` asserts the replace-in-place/
restart-timer behavior for repeated notices/errors, that the dismiss
control's reactive-but-non-focusable property holds (X20), and that the
ribbon stays visible for `recoverable` but hides for `critical`
(`ribbonVisibleForSeverity`). `accent.test.js` asserts the accent-color
fallback rule (untouched default and schema-absent both resolve to Ubuntu
orange; a genuine user choice, including blue, resolves to its own palette —
X25) and the reduced-motion query never throws (X26).

## 3a. Fast iteration with `dev-lab` (optional, development aid — not part of the shipped extension)

```sh
cd extensions/myna-shell
gjs -m dev-lab/main.js
```

**Expected**: a small libadwaita window opens with the wave-ribbon canvas, the
manual-override tuning controls (fake level slider, per-phase trigger buttons,
reduced-motion toggle, tunable sliders), and a plain text area. With
`myna-desktop` running, the ribbon reacts to genuinely live audio/state
(the tool reuses `dbus.js`'s `DictationService` unmodified — no simulated
data). Focus the text area and trigger a real session via the configured
hotkey to verify a spoken transcript lands there (confirms IBus injection
targets an ordinary `GtkTextView` the same as any other app, R20). Edit
`ribbon.js`/`ribbon-paint.js`/`accent.js`, relaunch (`Ctrl+C`, rerun the
command above) to see changes — no extension install/reload/relogin needed.
This step validates tuning only; it is not part of the extension's own
acceptance criteria (X1–X28) and has no pass/fail gate of its own.

## 4. Install & enable the extension — GNOME session

The UUID is listed in `enabledExtensions` of the `ubuntu` session mode
(`/usr/share/gnome-shell/modes/ubuntu.json`, from `gnome-shell-common`), so the
Shell loads it from the *system* datadir only - a `~/.local/share` copy is found
and skipped ("not loading … as part of session mode"). Install where the deb
lands, and remove the symlink before installing the real package:

```sh
UUID=myna-shell@canonical.com
sudo ln -sfn "$PWD/extensions/myna-shell" /usr/share/gnome-shell/extensions/$UUID
# dev-lab/ is a non-shipped development tool (R20) - never part of the
# packaged bundle (the symlink exposes it; `cp -r` + `rm -rf $UUID/dev-lab`
# mirrors the package layout exactly).
# Wayland: log out/in to reload the Shell (Alt+F2 r is X11-only).
```

Session-mode extensions are force-enabled: `gnome-extensions enable` is not
needed, and `gnome-extensions info $UUID` reports it as nonexistent rather than
listing it. Confirm it loaded with:

```sh
journalctl --user -b 0 -o cat | grep -i myna-shell   # no load errors
```

**Expected**: extension loaded, dormant (no overlay) because `myna-desktop`
is not yet running (X7).

## 5. End-to-end spoken run (the on-hardware acceptance)

```sh
myna-desktop --socket /tmp/myna.sock --language en &   # serves org.myna.Dictation
myna-desktop --install-shortcut '<Super>t>'                              # once: binds a shortcut (feature 003)
# focus a text field (GNOME Text Editor), then:
#   tap the shortcut  → HUD pill appears bottom-center (loading treatment if cold, then listening)
#   speak        → the wave ribbon flows, growing fuller/brighter with your voice
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
- The wave ribbon unfolds smoothly as the session starts, flows fuller/
  brighter as you speak at normal volume (calibrated to real speech, not a raw
  linear gain — R16a), relaxes to a thin idle line on a pause, and morphs into
  a few travelling dots once you stop, before briefly converging to a single
  point on completion (X14, X30, FR-010a/FR-010d, SC-004). The motion should
  feel smoothed and controlled — like fabric in a gentle airflow — rather
  than a nervous, tick-by-tick oscilloscope; individual syllables should
  still be visible, just without sharp jumps (FR-010, 2026-07-30 refinement).
- The ribbon is rendered in your system's accent color if you've actively
  chosen one, or Ubuntu orange otherwise (X27, FR-010b, SC-011) — try this
  with the accent-color setting on its untouched default, then with a color
  explicitly chosen, to see both fall/through paths.
- With the system's reduced-motion preference enabled, the ribbon is replaced
  by a static/minimally-animated alternative (X28, FR-022a, SC-012).
- On a successful completion, the ribbon briefly shows a quiet success
  indication before the pill clears, without delaying dismissal or a new
  session starting (X29, FR-010d).
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
- The wave ribbon stays **visible** (2026-07-30 refinement) — tinted amber,
  gently pulsing rather than tracking live input — instead of disappearing
  (X31, FR-010e, SC-014).
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
- The wave ribbon is **hidden** (X31, FR-010e) — unlike the recoverable case,
  the ribbon does not stay visible/tinted here.
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
`myna-desktop` not running, the button is dimmed (unavailable).

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
  never stolen by the HUD pill, with all states legible, the wave ribbon tracking
  voice/lifecycle phases correctly, and accent-color/reduced-motion behaving per
  X27/X28. **(2026-08-01, partial manual verification)**: the basic recording
  flow (HUD appears, focus never stolen, spoken text injected) is confirmed on
  hardware. Still open: the ribbon's accent-color/Ubuntu-orange fallback (X27),
  reduced-motion static alternative (X28), and the completion success pulse
  (X29) have not yet been specifically exercised.
- Steps 5a/5b pass: the recoverable notice auto-dismisses and never blocks a new
  session; the critical error persists until dismissed and its × control never
  steals focus. **(2026-08-01, manually verified on hardware — both the
  recoverable and critical severity walkthroughs pass.)**
- SC-013's structured comparison run and recorded (≥3 observers, majority
  verdict favoring the wave ribbon over the prior segmented meter — T050a).
- `docs/desktop-injection.md` §2 updated to record this extension as the GNOME
  focus-safe overlay answer (NotifyIndicator remains the fallback).
