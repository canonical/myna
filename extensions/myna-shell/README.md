# myna-shell — GNOME Shell dictation HUD

The focus-safe dictation indicator for GNOME (feature 004). A bottom-center
HUD pill, styled after GNOME's own volume/brightness OSD, that visualizes
`myna-desktop`'s dictation state and audio level. Pure UI: it never captures
audio, transcribes, or injects text — see `docs/desktop-injection.md` for the
last-mile that does. Contract and design history: `specs/004-gnome-shell-indicator/`.

## Install (development)

```sh
UUID=myna-shell@myna.dev
mkdir -p ~/.local/share/gnome-shell/extensions/$UUID
cp -r extensions/myna-shell/* ~/.local/share/gnome-shell/extensions/$UUID/
# dev-lab/ is a non-shipped development tool — never installed.
rm -rf ~/.local/share/gnome-shell/extensions/$UUID/dev-lab
gnome-extensions enable $UUID
```

GNOME Shell on Wayland does not hot-reload extension JS — after copying an
update, `gnome-extensions disable "$UUID" && gnome-extensions enable "$UUID"`
only refreshes `metadata.json`; a **log out / log back in** is required to
load changed module code. See `dev-lab/README.md` for a much faster
standalone iteration loop while tuning the wave ribbon specifically.

## What it shows

Driven entirely by `org.myna.Dictation` (served by `myna-desktop --dbus`):

- **Idle**: nothing — push-to-talk, no persistent overlay.
- **Loading / Recording / Transcribing / Finishing**: the pill with a filled
  mic icon and a state label.
- **Recoverable notice** (e.g. "No speech detected"): a non-blocking pill that
  auto-dismisses after ~3.5 s; a new session can start immediately.
- **Critical error** (e.g. "Microphone unavailable"): a persistent pill with a
  mic-with-slash icon and a dismiss (×) control — reactive but never
  keyboard-focusable, so dismissing it can never steal focus.
- **Audio level**: a flowing, accent-colored wave ribbon (2026-07-30 redesign
  — see `ribbon.js`) calibrated to real speech levels (not a raw linear
  gain): it unfolds when a session starts, flows with your voice, relaxes to
  a thin idle line on a pause, and morphs into a simplified processing
  motion when you stop. Colored from your system's accent-color preference,
  or Ubuntu orange if you haven't set one; falls back to a static line if
  you have reduced motion enabled.

## Layout

- `extension.js` — entry point: wires `dbus.js` → `states.js` → `view.js`.
- `dbus.js` — the `org.myna.Dictation` proxy + name-watch lifecycle. Zero
  Shell dependency (pure `Gio`/`GLib`) — reused verbatim by `dev-lab/`.
- `states.js` — pure wire-state → descriptor mapping (`{key, statusText,
  severity, hidden}`); the stable, unit-tested contract layer.
- `view.js` — the `IndicatorView` seam. A redesign replaces one file
  (`hud.js`) and this factory; nothing else moves.
- `hud.js` + `hud-logic.js` — the current view: `hud.js` is the Shell/Clutter
  actor (harness-tier, manual-acceptance only — GNOME Shell extensions can't
  run headless); `hud-logic.js` is the pure, unit-tested logic factored out of
  it (positioning, icon/colour choice, auto-dismiss/replace-in-place rules,
  and which state transitions force a wave-ribbon phase change).
- `vumeter.js` — pure RMS/peak → calibrated loudness envelope + stale-decay;
  reused unchanged by `ribbon.js`.
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
- `test/*.test.js` — headless GJS tests (`gjs -m test/<name>.test.js`) for
  everything above except `hud.js` itself.

## Testing

```sh
cd extensions/myna-shell
for f in test/*.test.js; do gjs -m "$f"; done
```

Pure logic (`states.js`, `vumeter.js`, `ribbon.js`, `accent.js`,
`hud-logic.js`, `dbus.js`'s lifecycle) is unit-tested headless — including a
real headless-Cairo smoke check of `ribbon-paint.js` (an `ImageSurface`
needs no display server). The actual widget tree in `hud.js` cannot be
tested this way — GNOME Shell's Clutter fork aborts if you construct an
actor outside a running compositor — so its rendering is manual-acceptance
only; see `specs/004-gnome-shell-indicator/quickstart.md`.

## Out of scope

Text injection, model/mic selection, translation, transcript display, and
screen-reader announcements (tracked separately, plan T56) are all out of
scope for this extension. Public distribution (extensions.gnome.org review,
Ubuntu archive, or bundling in a snap) is noted as follow-up, not delivered
here — install today by copying the bundle in-tree per above.
