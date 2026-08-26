# Quickstart / Validation: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30; architecture revision: 2026-08-26)

Runnable validation that the focus-safe hosted indicator works end-to-end. See
`contracts/` for the guarantees each step proves and `plan.md` for structure.
**(2026-08-26)**: the pill is drawn by the `myna-hud` GTK4 application
(GPU/GLSL-only), hosted by the extension; the dev labs are now modes of the
same binary (`--lab`, `--serve-dbus`). Steps below exercise all three
deliverables and both severities.

## Prerequisites

- Ubuntu 26.04+ on **Wayland**, GNOME Shell **50 or 51** (`gnome-shell --version`),
  GTK ≥ 4.22, libadwaita ≥ 1.7 (the accent-color style-manager API).
  Dev-loop note (2026-07-21, verified on Shell 50.2): mutter **removed the
  nested backend** — there is no `gnome-shell --nested`, plain `--wayland` does
  not nest, and the `--devkit` viewer (`mutter-devkit`) is not shipped by
  Ubuntu's mutter packages. On Wayland the only code-reload of the *host* is a
  Shell restart (log out/in); iterate on the *renderer* via `--lab`
  (sub-second restart) and on host logic via the GJS contract tests (step 3).
- The Workshop dev env (`.workshop/myna.yaml` — client/renderer SDKs — and
  `.workshop/myna-shell.yaml` — gjs + gnome-shell) — see the foundational
  task; or a GNOME session on hardware.
- A Rust toolchain for `client/` (the renderer and publisher suites).
- For the full spoken run: a running inference backend, e.g.
  `myna-server --adapter whisper --socket /tmp/myna.sock`, and IBus running
  (feature 003) for text injection. Not needed for steps 1–4 or the lab.

## 1. Hermetic publisher + renderer tests (no bus, no display) — Rust, TDD

```sh
cd client
cargo test -p myna-desktop dbus_indicator     # C2–C7, C10–C11, P1–P12, P16–P19 (fake bus)
cargo test -p myna-desktop controller         # completion_indicator_state, empty-transcript → notice
cargo test -p myna-desktop policy             # P20–P23: presence-driven fallback suppression (fake presence)
cargo test -p myna-hud                        # states/vumeter/ribbon/shader/hud_logic/input_region/
                                              # dbus_consumer (fake proxy)/simulator — the ported
                                              # contract suites, now Rust-tier
```

**Expected**: the state→`State`-string mapping (incl. loading/recording and
notice/error severity splits), content-free payloads, level throttling, and
`DbusTrigger` dedup pass; the launcher policy suppresses/restores the
notification fallback on presence changes (P20–P22) and never blocks
dictation on bus errors; `myna-hud`'s pure modules match the ported GJS
contract suites (descriptor mapping, envelope ballistics + arrival-time stale
decay, phase timings incl. morph-dots/complete-convergence, shader `#define`
conformance and uniform packing, input-region geometry, fake-proxy consumer
lifecycle, simulator mapping). All written red-first (constitution I).

## 2. Env-gated integration (real session bus / real GL driver) — VM or hardware

```sh
cd client
MYNA_DBUS_TESTS=1 dbus-run-session -- cargo test -p myna-desktop dbus_hw   # C1, C9, C12, C13, P13–P14
MYNA_HUD_GL_TESTS=1 cargo test -p myna-hud --test render_gl               # surfaceless-EGL shader check
```

**Expected**: `myna-desktop` claims `org.myna.Dictation`, a `zbus` client
observes `PropertiesChanged` + properties, name-appeared/vanished fire, and
the presence seam round-trips `org.myna.Shell` ownership; the render check
compiles the generated shader on the real driver and rasterizes non-blank,
non-flooded, per-phase-distinct frames (port of the former Python
`render_headless.py`). Runs identically on the desktop VM and hardware
(constitution II).

## 3. GJS host contract tests (pure host logic) — no Shell

```sh
cd extensions/myna-shell
gjs -m test/place.test.js
gjs -m test/respawn.test.js
gjs -m test/resolve.test.js
gjs -m test/presence.test.js
gjs -m test/host.test.js
```

**Expected**: bottom-center placement math across monitor layouts (XH1),
binary-resolution order + failure states (XH2), bounded-backoff respawn with
restart budget (XH3), adoption idempotence (XH4), and presence-name lifecycle
against a stub bus (XH5). The old drawing-module suites (states/hud/ribbon/
accent/lifecycle/vumeter) are gone — their assertions live in `cargo test -p
myna-hud` now (step 1).

## 3a. Fast iteration with the lab — no backend, no Shell

```sh
cd client
cargo run -p myna-hud -- --lab
```

**Expected**: a normal focusable window with the wave-ribbon canvas, manual
controls (state, severity, level, reduced-motion, phase triggers) and a plain
text area — driving the *identical* renderer modules with **no backend and no
`myna-desktop` required**. Edit the pure modules, restart in seconds. This is
the replacement for the old `dev-lab`/`dev-lab-gpu` pair (R25); it carries no
acceptance weight of its own.

## 3b. Backend simulation — drive the real hosted indicator without a backend

```sh
cd client
cargo run -p myna-hud -- --serve-dbus    # claims org.myna.Dictation
```

**Expected**: with the extension installed (step 4) and the simulator owning
`org.myna.Dictation`, the hosted pill reacts to the simulator's state/level
controls — the full extension→spawn→adopt→position→render→consume chain is
exercised with zero backend. The simulator never takes the name by force and
releases it cleanly on exit (port of the former `dictation_service.py`
behavior).

## 4. Install & enable the extension — GNOME session

The UUID is listed in `enabledExtensions` of the `ubuntu` session mode
(`/usr/share/gnome-shell/modes/ubuntu.json`, from `gnome-shell-common`), so the
Shell loads it from the *system* datadir only - a `~/.local/share` copy is found
and skipped ("not loading … as part of session mode"). Install where the deb
lands, and remove the symlink before installing the real package:

```sh
UUID=myna-shell@canonical.com
sudo ln -sfn "$PWD/extensions/myna-shell" /usr/share/gnome-shell/extensions/$UUID
# The renderer binary must be resolvable. For a dev build:
export MYNA_HUD_BINARY="$PWD/client/target/debug/myna-hud"
# (installed snap: /snap/bin/myna-hud — resolved automatically; deb: /usr/bin/myna-hud)
# Wayland: log out/in to reload the Shell (Alt+F2 r is X11-only).
```

Session-mode extensions are force-enabled: `gnome-extensions enable` is not
needed. Confirm it loaded with:

```sh
journalctl --user -b 0 -o cat | grep -i myna-shell   # no load errors
busctl --user status org.myna.Shell >/dev/null && echo "presence: up"
pgrep -af myna-hud                                    # the host spawned the renderer
```

**Expected**: extension loaded, `org.myna.Shell` owned, `myna-hud` running
with no visible window (idle → nothing shown, XH6/XH8 dormant path).

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
- HUD pill appears within ~100–200 ms of start and clears after stop (XH10
  timing path, SC-003).
- **Focus is never stolen**: while the pill is visible, keep typing in the
  field — characters land there (XH10, SC-001). *This is the whole point of
  the feature.*
- **Clicks pass through**: click where the pill is — the click lands in the
  app underneath, not on the pill (XH12, FR-025, SC-015).
- The pill has no taskbar/alt-tab entry, shows on every workspace, stays
  above normal windows, and cannot be moved, minimized, or closed by ordinary
  window-management means (XH10, FR-024, SC-015); it sits bottom-center of the
  primary monitor matching GNOME's own OSD position, including after monitor
  changes (XH11).
- States are distinct: a cold model load shows the **loading** treatment, not
  the listening treatment (renderer guarantee — FR-006, SC-002).
- The wave ribbon unfolds smoothly as the session starts, flows fuller/
  brighter as you speak at normal volume (calibrated to real speech — R16a),
  relaxes to a thin idle line on a pause, and morphs into a few travelling
  dots once you stop, before briefly converging to a single point on
  completion (renderer guarantees — SC-004).
- The ribbon is rendered in your system's accent color if you've actively
  chosen one, or Ubuntu orange otherwise (FR-010b, SC-011) — try the
  untouched default, then an explicitly chosen color. Live changes re-color
  without restart.
- With the system's reduced-motion preference enabled, the ribbon is replaced
  by a static/minimally-animated alternative (FR-022a, SC-012) — and toggling
  it must never crash the app (E2b's absent-safe sourcing).
- The injected transcript matches what you said (feature 003 unchanged).
- No transcript text ever appears in the pill or in logs (FR-012, SC-005).

## 5a. Recoverable-issue walkthrough (US2a)

```sh
# tap the shortcut, then tap again immediately without speaking:
#   tap → tap (no speech in between)
```

**Expected / assert**: a non-blocking notice appears (e.g. "No speech
detected") with the filled mic icon; the ribbon stays **visible**, tinted
amber, gently pulsing (FR-010e, SC-014); the notice clears on its own after
~3.5 s; a new session starts immediately without being blocked; a second
"no speech" while the first is showing replaces in place and restarts the
timer (FR-007a/FR-007d, R15). Clicks outside the (absent) dismiss control pass through.

## 5b. Critical-error walkthrough (US2a)

```sh
# simulate a hard failure, e.g. unplug/disable the microphone, or stop the
# backend mid-session, then tap the shortcut.
```

**Expected / assert**: a persistent notice appears with a clear, content-free
reason, a mic-with-slash icon, a visible dismiss (×) control, and the ribbon
**hidden** (FR-010e). The notice does not clear on its own. **Only** the × is
clickable (the input region covers exactly its rectangle — clicks elsewhere on
the pill pass through, FR-025); clicking it clears the notice immediately.
Keep a text field focused while clicking the ×: typing continues to land in
the field (FR-007c — the window never takes keyboard focus; if this ever
regresses, the documented fallback is a visual-only × with dismiss via a new
session/`Stop()`, R22). A second critical error replaces in place without
waiving the dismiss (FR-007d, R15).

## 6. Panel toggle (optional, P3 — unchanged, future)

With the panel button enabled: click it → a session starts (HUD pill appears); click
again → it stops and commits, identical to the hotkey (FR-014, SC-010). With
`myna-desktop` not running, the button is dimmed (unavailable).

## 7. Robustness spot-checks (edge cases)

```sh
# daemon crash mid-session:
kill %1        # while a session is active → pill clears to idle (renderer), no error spew
# renderer crash while hosted:
pkill -f myna-hud    # → the host respawns it within the backoff; indicator returns (XH8, SC-016)
# permanently-crashing binary (point MYNA_HUD_BINARY at /bin/false):
#   → restart budget exhausts, host goes dormant, logs once — never a tight crash loop (XH3)
# disable cleanliness:
gnome-extensions disable $UUID     # renderer terminated, presence name released, no orphans (XH6/XH9)
```

## 8. Watermarks — constitution III

```sh
cd client
cargo test -p myna-desktop --test watermarks    # publisher: level-pump cadence etc. (unchanged)
cargo test -p myna-hud --test watermarks        # renderer: activation→visible, envelope constants,
                                                #   GLArea frame budget
```

**Expected**: publisher cadence within declared tolerances (no capture-path
regression vs the 002/003 baselines); renderer watermarks within tolerance on
reference hardware.

## Done when

- Steps 1–3 green (hermetic publisher + renderer, gated integration, host
  contract tests).
- Steps 3a/3b usable: the lab renders with no backend; the simulator drives
  the hosted indicator end-to-end without `myna-desktop`.
- Step 5 passes on hardware: spoken text lands in the focused app **and**
  focus is never stolen by the hosted pill, clicks pass through, the window
  is absent from window lists / present on all workspaces / always on top,
  with all states legible, the wave ribbon tracking voice/lifecycle phases
  correctly, and accent-color/reduced-motion behaving per FR-010b/FR-022a
  (including the no-crash check when motion settings are toggled or absent).
- Steps 5a/5b pass: the recoverable notice auto-dismisses and never blocks a
  new session; the critical error persists until dismissed, its × control is
  the only interactive pixel, and it never steals focus.
- Step 7 passes: renderer crash → bounded respawn (or budgeted dormancy);
  disable → clean teardown, no orphaned windows or processes.
- `docs/desktop-injection.md` §2 and Future updated to record the
  extension-hosted overlay as the GNOME focus-safe answer (the
  "no sanctioned way" claim now qualified for unassisted clients only).
