# myna-shell — GNOME Shell overlay host for the myna dictation HUD

A thin GNOME Shell extension that hosts the **myna-hud** renderer
application as a focus-safe overlay (feature 004). It does **not** draw the
HUD or consume `com.canonical.Myna.Dictation` itself — the standalone
`myna-hud` binary (see `client/myna-hud`) does both. This extension:

- launches the renderer (`Meta.WaylandClient.new_subprocess`, so the child
  inherits the compositor's Wayland socket),
- adopts its window (`owns_window` → DOCK type, hidden from the window
  list, on all workspaces, above normal windows, never focused),
- positions it bottom-centre of the primary work area (and keeps it clear
  of an auto-hide bottom dash-to-dock via `Main.layoutManager.dashToDockStruts`),
- supervises it (respawn on unexpected exit with bounded backoff, terminate
  on disable), and
- owns `com.canonical.Myna.Shell` for as long as it is enabled, so `myna-desktop` can
  suppress its own fallback notification indicator (C12/C13).

The HUD pill itself, its GPU wave ribbon, accent colour, reduced-motion
handling, lab and simulator modes all live in `client/myna-hud`. Contract
and design history: `specs/004-gnome-shell-indicator/`.

## Install (development)

```sh
UUID=myna-shell@canonical.com
mkdir -p ~/.local/share/gnome-shell/extensions/$UUID
cp -r extensions/myna-shell/* ~/.local/share/gnome-shell/extensions/$UUID/
gnome-extensions enable $UUID
```

The renderer must be reachable as `snap run myna.hud` (the packaged snap
app), or as `$MYNA_HUD_BINARY` (a locally built binary) for development. If
neither resolves, the extension logs once and stays dormant — it never
crash-loops.

GNOME Shell on Wayland does not hot-reload extension JS — after copying an
update, `gnome-extensions disable "$UUID" && gnome-extensions enable "$UUID"`
only refreshes `metadata.json`; a **log out / log back in** is required to
load changed module code. To iterate on the renderer without the extension,
run `client/myna-hud` directly (`--lab` for a standalone HUD, `--serve-dbus`
to publish a simulated `com.canonical.Myna.Dictation`).

## What the hosted overlay shows

Driven entirely by `com.canonical.Myna.Dictation` (served by `myna-desktop`):

- **Idle**: nothing — push-to-talk, no persistent overlay.
- **Loading / Recording / Transcribing / Finishing**: a bottom-centre pill
  with a filled mic icon and a state label.
- **Recoverable notice** (e.g. "No speech detected"): a non-blocking pill
  that auto-dismisses after ~3.5 s; a new session can start immediately.
- **Critical error** (e.g. "Microphone unavailable"): a persistent pill with
  a mic-with-slash icon that does not clear on a timer — the client resolves
  it by publishing a different state.
- **Audio level**: a flowing, accent-coloured GPU wave ribbon, calibrated to
  real speech levels, unfolding on session start, flowing with the voice,
  relaxing on a pause, and morphing into a simplified processing motion on
  stop. Coloured from the desktop's accent preference (Yaru-aware), or
  Ubuntu orange as a fallback; a static line under reduced motion.

## Layout

- `extension.js` — entry point: wires the host and the `com.canonical.Myna.Shell`
  presence name.
- `host.js` — the stateful glue: spawn, adopt, dock-type, position,
  supervise. Composes the pure modules below.
- `place.js` — pure placement math (bottom-centre + shrink-above-dock).
- `resolve.js` — pure launch resolution (`$MYNA_HUD_BINARY` →
  `snap run myna.hud`).
- `respawn.js` — pure respawn policy (bounded backoff → dormancy).
- `presence.js` — owns `com.canonical.Myna.Shell` for exactly as long as enabled,
  fail-soft.
- `dockStrutsConsumer.js` — follows `Main.layoutManager.dashToDockStruts`
  (the dash-to-dock reserved-extent export) so the pill is never covered by
  an auto-hide bottom dock.
- `metadata.json` — declares Shell 50/51.
- `test/*.test.js` — headless GJS contract tests (`gjs -m test/<name>.test.js`)
  for the pure modules above; no Shell needed.

## Compositor behaviour

- **Launch through `Meta.WaylandClient`** so the renderer inherits the
  compositor's Wayland socket (the child connects via `WAYLAND_SOCKET`, the
  normal path for a confined GTK app) and its window can be adopted with
  `owns_window`.
- **Adoption on `map`** (the `window_manager` signal DIN uses), with
  `owns_window` guarded against the X11-window exception; a window that
  unmaps at idle and re-maps is re-adopted, and an `unmanaged` handler
  clears tracking so the fresh window is adopted rather than rejected.
- **Focus safety**: the adopted window is DOCK-typed (mutter forces
  `takes_focus = FALSE`), never focused on map, and the renderer's surface
  input region is empty in every state — typing into the focused
  application is never interrupted.
- **Overview**: the window's actor is reparented into
  `Main.layoutManager.uiGroup` while the overview shows, so the HUD persists
  over it (the dock mechanism).
- **No synchronous work on the main loop** that isn't drawing; all signal
  handlers are owner-tracked via `connectObject`/`disconnectObject`, so
  teardown cannot leak.

The live compositor behaviour (dock typing, focus safety, click-through,
repositioning) is verified on hardware, not headlessly — the pure modules
are unit-tested, and the integration is exercised by the on-hardware run
(T125 / `specs/004-gnome-shell-indicator/quickstart.md`).

## Testing

```sh
cd extensions/myna-shell
test/run-suite.sh          # everything below, in one go
```

`test/run-suite.sh` runs the pure GJS contract suites (`test/*.test.js`),
which need nothing but `gjs`. It runs in CI as `make test-extension`, in its
own Workshop (`.workshop/myna-shell.yaml`).

`test/next-shell.sh` runs the same suite inside a throwaway LXD container of
Ubuntu 26.10 (Shell 51), since the workshop base only reaches Shell 50.
`make test-extension-next`; CI runs it as `extension-next`,
`continue-on-error` — it tracks a development series, so it reports without
gating a merge on someone else's upload.

Geometry, colour, focus safety and the dock interaction stay
manual-acceptance; see `specs/004-gnome-shell-indicator/quickstart.md`.

## Out of scope

Text injection, model/mic selection, translation, transcript display, and
screen-reader announcements (tracked separately) are out of scope for this
extension; the HUD rendering itself is the `myna-hud` application, not this
extension. Public distribution (extensions.gnome.org review, Ubuntu archive,
or bundling in a snap) is noted as follow-up, not delivered here — install
today by copying the bundle in-tree per above.
