# myna-snap

The UbuSTT dictation **client** snap — ships the Rust orchestrator
(`client/myna-desktop`, the push-to-talk app, plus the `myna-dictate`
testbed CLI). Feature `005-myna-orchestrator-snap`
(`specs/005-myna-orchestrator-snap/`); plan task T57.

This snap is the mirror image of the inference snaps: **it** owns the
microphone, the hotkey, and text injection (audio-push invariant); the
backend snaps only receive PCM on a socket. It deliberately has **no
`network` plug** — every boundary is a Unix socket or the session bus.

## Setup (the repeatable path)

```shell
# 0. A backend snap must be installed and serving, e.g. whisper (see
#    whisper-snap/README.md); check with:
snap logs -n5 whisper.server

# 1. Build + install this snap
./dev/prepare.sh && snapcraft pack
sudo snap install --dangerous ./myna_*.snap

# 2. Connect the two manual interfaces
sudo snap connect myna:pipewire                          # mic capture (snapd gates it)
sudo snap connect myna:backend whisper:ubustt-socket     # the backend session socket

# 3. Bind your dictation key (writes <accel> → /snap/bin/myna.toggle via dconf)
myna.install-shortcut '<Super>t>'                 # or any other accel string

# 4. Run the daemon (leave it running; autostart is a known gap below)
myna

# 5. Focus a text field, tap the key, speak, tap again → transcript injected.
```

That's it. If step 5 misbehaves, jump to **Troubleshooting**.

The `backend` plug is a writable content share of the backend snap's
`$SNAP_COMMON/run` (T14c): after connecting, the session socket appears at
`/var/snap/myna/current/backend/run/ubustt.sock`. One backend at a time
(whisper / nemotron / qwen provide the same slot; multi-backend selection
is T48). The backend daemon must have run at least once for the socket to
exist (`sudo snap start whisper.server`).

## Activation

Everything is **press-to-toggle** by default: tap the key to start, tap
again to stop. Two trigger transports:

- **Control socket (default)** — `myna` listens for pokes; `myna.toggle`
  sends one. Any desktop can bind a custom shortcut to
  `/snap/bin/myna.toggle` (`myna.install-shortcut` does it for GNOME).
- **GlobalShortcuts portal** (`MYNA_ACTIVATION=portal myna`) — the
  sandboxed-native trigger. On xdg-desktop-portal-gnome 51 (GNOME Shell
  51.alpha) the bind is auto-accepted: no sheet, the grab registers at
  daemon start (verified 2026-08-18). Older backends may show a bind sheet
  once per start - portal v1 has no persist/restore token, and ashpd 0.13
  doesn't expose v2's. Either way it's press-to-toggle like everything
  else; `myna --hold` (or `MYNA_ACTIVATION=portal myna --hold`) switches
  it to hold-to-talk.

`MYNA_ACTIVATION=stdin myna` drives from the terminal (debug; injects back
into the terminal).

**Indicator**: the launcher always serves `org.myna.Dictation` for the
myna-shell GNOME extension; desktop notifications are the fallback. The
experimental GTK overlay is `--overlay`.

**Env knobs**: `MYNA_BACKEND_SOCKET`, `MYNA_ACTIVATION`, `MYNA_LANGUAGE`.

## Apps

| app | what |
|---|---|
| `myna` | the dictation daemon (launcher around `myna-desktop`) |
| `myna.toggle` | poke the daemon's control socket (start/stop) |
| `myna.install-shortcut` | bind a GNOME custom shortcut → `myna.toggle` (dconf) |
| `myna.testbed` | the `myna-dictate` testbed CLI (`--list-devices`, `--clip`, `--dialect`, …) |

## Verify (confined, end to end)

```shell
# 1. testbed round-trip through the content-shared socket
myna.testbed --socket /var/snap/myna/current/backend/run/ubustt.sock \
    --language en --clip ~/path/to/clip.wav

# 2. device enumeration over the confined PipeWire socket
myna.testbed --list-devices

# 3. daemon + bus: org.myna.Dictation is owned while `myna` runs
gdbus introspect --session --dest org.myna.Dictation \
    --object-path /org/myna/Dictation
```

## Troubleshooting

- **`myna` says "no backend socket"** — connect the backend plug (step 2)
  and make sure the backend daemon has run (`snap logs whisper.server`).
- **`myna.toggle` can't reach the daemon** — `myna` isn't running (or it's
  running with a different `MYNA_ACTIVATION`; control mode is required).
- **Nothing is injected, state shows `error`** — read the reason:
  `gdbus call --session --dest org.myna.Dictation \
    --object-path /org/myna/Dictation \
    --method org.freedesktop.DBus.Properties.Get org.myna.Dictation ErrorMessage`
  (a *capture_failed* usually means `myna:pipewire` isn't connected).
- **A press "does nothing" - the session starts and dies silently** - with
  the default `--dbus` wiring, ALL feedback (including errors) goes to
  `org.myna.Dictation` properties; without the myna-shell extension nothing
  renders it (notifications are only the fallback when the bus can't be
  owned). Critical session errors are always printed to the daemon's
  stderr, so run `myna` from a terminal and read them there. For the full
  stage-by-stage trace add `MYNA_DEBUG=1` (`ctrl`/`capture`/`ws`/`inject`
  lines - where the trail stops is the culprit). A
  `pipewire: mod.client-node: detected old client version 5` journal line
  at press time is benign: it's the snap-staged (older) libpipewire
  connecting, and since capture starts only per press, it proves the
  hotkey fired. Classic silent-death cause: `--socket` /
  `MYNA_BACKEND_SOCKET` pointing at a backend snap's
  `/var/snap/<snap>/common/run/...` directly - confinement denies it (the
  `backend` content share exists precisely for this); the denial shows in
  `sudo journalctl -k`. Live state without restarting: read the
  `State`/`ErrorMessage` properties as above.
- **`busctl` fails with "Operation not permitted" / "Access denied" against
  the session bus in general** — your shell's `DBUS_SESSION_BUS_ADDRESS`
  carries a stale `guid=` (a terminal/tmux server that survived a logout;
  sd-bus validates the guid, GIO ignores it, myna recovers by itself). Fix:
  `export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"`, and
  restart the offending terminal server.
- **Reading the bus from a container fails with "Access denied"** — snapd's
  `dbus` slot only admits `label=unconfined` peers; call from a host shell
  (not `snap run --shell`, Workshop/LXD/toolbox). The GNOME Shell extension
  is in-compositor (unconfined) and unaffected.

## Interfaces (and why)

| plug | why |
|---|---|
| `pipewire` | native PipeWire capture (`/run/user/*/pipewire-0`) |
| `desktop` | GlobalShortcuts portal + desktop notifications |
| `desktop-legacy` | the IBus daemon's private socket (text injection) |
| `gsettings` | dconf write for `myna.install-shortcut` |
| `wayland`, `x11` | the GTK indicator window |
| `backend` (content) | the backend session socket |
| slot `org.myna.Dictation` (dbus) | the indicator publisher (state + level only) |

The IBus injector finds the daemon's address file under your *real* home
even though snapd redirects `$HOME` (feature-005 discovery fix); the
control socket lives under the snap-scoped `$XDG_RUNTIME_DIR`.

**Confinement note (indicator bus):** `org.myna.Dictation` is properties-only
by design. snapd's `dbus` slot AppArmor policy denies broadcasting *custom*
signals to unconfined subscribers (and can't be safely widened — AppArmor
dbus rules can't discriminate message types), but it does allow
`org.freedesktop.DBus.Properties` sends on the service's own path, which is
exactly the shape of a `PropertiesChanged` broadcast. State + level updates
are therefore pushed with standard `PropertiesChanged`; the myna-shell
extension subscribes and gets the fast push path confined or not — no
polling (contract `specs/004-gnome-shell-indicator/contracts/dbus-interface.md`
§Confinement).

## Known gaps (tracked)

- No autostart on login yet (snapd user daemons are still experimental);
  start `myna` from a terminal or Startup Applications.
- Socket access control is "an admin connected the plug" — identity/polkit
  is T17.
- Store name `myna` is unregistered as of 2026-07-22; register before any
  store upload.
- **Portal hotkey:** if a bound key doesn't grab, diagnose with:
  ```shell
  gdbus monitor --session --dest org.freedesktop.portal.Desktop &
  MYNA_ACTIVATION=portal myna   # then press your key
  # org.freedesktop.portal.GlobalShortcuts Activated should appear on press;
  # nothing = the grab never registered (portal side)
  ```
