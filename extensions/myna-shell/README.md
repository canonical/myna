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
gnome-extensions enable $UUID
```

GNOME Shell on Wayland does not hot-reload extension JS — after copying an
update, `gnome-extensions disable "$UUID" && gnome-extensions enable "$UUID"`
only refreshes `metadata.json`; a **log out / log back in** is required to
load changed module code.

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
- **Audio level**: a segmented green/yellow/red VU meter, calibrated to real
  speech levels (not a raw linear gain) — see `vumeter.js`.

## Layout

- `extension.js` — entry point: wires `dbus.js` → `states.js` → `view.js`.
- `dbus.js` — the `org.myna.Dictation` proxy + name-watch lifecycle.
- `states.js` — pure wire-state → descriptor mapping (`{key, statusText,
  severity, hidden}`); the stable, unit-tested contract layer.
- `view.js` — the `IndicatorView` seam. A redesign replaces one file
  (`hud.js`) and this factory; nothing else moves.
- `hud.js` + `hud-logic.js` — the current view: `hud.js` is the Shell/Clutter
  actor (harness-tier, manual-acceptance only — GNOME Shell extensions can't
  run headless); `hud-logic.js` is the pure, unit-tested logic factored out of
  it (positioning, icon/colour choice, auto-dismiss/replace-in-place rules).
- `vumeter.js` — pure RMS/peak → VU intensity, decay, and colour-zone mapping.
- `stylesheet.css` — pill/icon/label/VU styling, including the severity and
  high-contrast colour classes.
- `test/*.test.js` — headless GJS tests (`gjs -m test/<name>.test.js`) for
  everything above except `hud.js` itself.

## Testing

```sh
cd extensions/myna-shell
for f in test/*.test.js; do gjs -m "$f"; done
```

Pure logic (`states.js`, `vumeter.js`, `hud-logic.js`, `dbus.js`'s lifecycle)
is unit-tested headless. The actual widget tree in `hud.js` cannot be — GNOME
Shell's Clutter fork aborts if you construct an actor outside a running
compositor — so its rendering is manual-acceptance only; see
`specs/004-gnome-shell-indicator/quickstart.md`.

## Out of scope

Text injection, model/mic selection, translation, transcript display, and
screen-reader announcements (tracked separately, plan T56) are all out of
scope for this extension.
