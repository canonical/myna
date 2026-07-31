# myna-shell — GNOME Shell dictation HUD

The focus-safe dictation indicator for GNOME (features 004 and 009). A bottom-center
HUD pill, styled after GNOME's own volume/brightness OSD, that visualizes
`myna-desktop`'s dictation state and audio level. Pure UI: it never captures
audio, transcribes, or injects text — see `docs/desktop-injection.md` for the
last-mile that does. Current switchable-HUD contract:
`specs/009-switchable-basic-hud/`; design history:
`specs/004-gnome-shell-indicator/`.

## Install (development)

```sh
UUID=myna-shell@myna.dev
mkdir -p ~/.local/share/gnome-shell/extensions/$UUID
cp -r extensions/myna-shell/* ~/.local/share/gnome-shell/extensions/$UUID/
# dev-lab/ is a non-shipped development tool — never installed.
rm -rf ~/.local/share/gnome-shell/extensions/$UUID/dev-lab
glib-compile-schemas --strict ~/.local/share/gnome-shell/extensions/$UUID/schemas
gnome-extensions enable $UUID
gnome-extensions prefs $UUID
```

The preference defaults to **Basic** and persists per user. Choose **Wave
ribbon** in preferences; changes apply live without restarting Shell or
interrupting dictation.

GNOME Shell on Wayland does not hot-reload extension JS — after copying an
update, `gnome-extensions disable "$UUID" && gnome-extensions enable "$UUID"`
only refreshes `metadata.json`; a **log out / log back in** is required to
load changed module code. See `dev-lab/README.md` for a much faster
standalone iteration loop while tuning the wave ribbon specifically.

Verify the installed extension after logging back in:

```sh
gnome-extensions info myna-shell@myna.dev
gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
  --method org.gnome.Shell.Extensions.GetExtensionErrors myna-shell@myna.dev
```

Expected: `State: ACTIVE` and an empty error list. `State: ERROR` with a missing
`schemas/gschemas.compiled` means the install skipped `glib-compile-schemas`;
compile the installed `schemas/` directory, then log out/in because Shell keeps
startup failures for the lifetime of the Wayland session.

## What it shows

Driven entirely by `org.myna.Dictation` (served by `myna-desktop --dbus`):

- **Idle**: nothing — push-to-talk, no persistent overlay.
- **Loading / Listening / Finishing**: the ordinary push-to-talk lifecycle.
  The view also supports a distinct **Transcribing** state if a publisher sends
  it; the current desktop controller normally keeps showing Listening while the
  trigger is held and moves directly to Finishing on release.
- **Recoverable notice** (e.g. "No speech detected"): a non-blocking pill that
  auto-dismisses after ~3.5 s; a new session can start immediately.
- **Critical error** (e.g. "Microphone unavailable"): a persistent pill with a
  mic-with-slash icon and a dismiss (×) control — reactive but never
  keyboard-focusable, so dismissing it can never steal focus.
- **Basic audio level (default)**: a simple horizontal progress bar calibrated
  to normal speech. It responds only while listening and decays fully to empty.
- **Wave ribbon**: the existing flowing, accent-colored wave ribbon (2026-07-30 redesign
  — see `ribbon.js`) calibrated to real speech levels (not a raw linear
  gain): it unfolds when a session starts, flows with your voice, relaxes to
  a thin idle line on a pause, and morphs into a simplified processing
  motion when you stop. Colored from your system's accent-color preference,
  or Ubuntu orange if you haven't set one; falls back to a static line if
  you have reduced motion enabled.

## Layout

- `extension.js` — entry point: wires settings + `dbus.js` → `states.js` →
  `indicator-controller.js`.
- `indicator-controller.js` — owns state, held notices, timestamped levels, and
  atomic view replacement.
- `dbus.js` — the `org.myna.Dictation` proxy + name-watch lifecycle. Zero
  Shell dependency (pure `Gio`/`GLib`) — reused verbatim by `dev-lab/`.
- `states.js` — pure wire-state → descriptor mapping (`{key, statusText,
  severity, hidden}`); the stable, unit-tested contract layer.
- `view-selection.js` — pure style normalization and constructor selection;
  `view.js` supplies the real Basic/Wave Shell actors.
- `basic.js` + `basic-logic.js` — basic HUD actor and pure meter logic.
- `hud.js` + `hud-logic.js` — the Wave view: `hud.js` is the Shell/Clutter
  actor (harness-tier, manual-acceptance only — GNOME Shell extensions can't
  run headless); `hud-logic.js` is the pure, unit-tested logic factored out of
  it (positioning, icon/colour choice, and wave-ribbon phase decisions).
- `vumeter.js` — pure RMS/peak → calibrated loudness envelope + stale-decay;
  feeds both `basic-logic.js` and `ribbon.js`.
- `ribbon.js` — pure wave-ribbon strand/control-point generation and the 5
  lifecycle-phase timing functions (unfold/flow/relax/morph/complete).
- `accent.js` — pure accent-color/reduced-motion resolution from GNOME's
  `org.gnome.desktop.interface` GSettings, plus the live `SystemPreferences`
  reader.
- `ribbon-paint.js` — the shared Cairo drawing function, toolkit-agnostic
  (no Shell/Gtk import) — used unmodified by both `hud.js` and `dev-lab/`.
- `stylesheet.css` — pill/icon/label/ribbon styling, including the severity
  and high-contrast colour classes.
- `dev-lab/` — a standalone GTK4+libadwaita tuning app for the wave ribbon,
  **not part of the shipped bundle** (see `dev-lab/README.md`).
- `test/*.test.js` — headless GJS tests (`gjs -m test/<name>.test.js`) for the
  pure logic and lifecycle seams. The actor trees in `basic.js` and `hud.js`
  require an installed Shell acceptance run.

## Testing

```sh
cd extensions/myna-shell
for f in test/*.test.js; do gjs -m "$f"; done
```

Pure logic (`states.js`, `vumeter.js`, `basic-logic.js`, `view-selection.js`,
`indicator-controller.js`, `ribbon.js`, `accent.js`, `hud-logic.js`, and
`dbus.js`'s lifecycle) is unit-tested headless — including a
real headless-Cairo smoke check of `ribbon-paint.js` (an `ImageSurface`
needs no display server). The widget trees in `basic.js` and `hud.js` cannot
be tested this way — GNOME Shell's Clutter fork aborts if you construct an
actor outside a running compositor — so their rendering is manual-acceptance
only; see `specs/009-switchable-basic-hud/quickstart.md`.

## Out of scope

Text injection, model/mic selection, translation, transcript display, and
screen-reader announcements (tracked separately, plan T56) are all out of
scope for this extension. Public distribution (extensions.gnome.org review,
Ubuntu archive, or bundling in a snap) is noted as follow-up, not delivered
here — install today by copying the bundle in-tree per above.
