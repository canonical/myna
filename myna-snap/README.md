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
snap logs -n5 myna-whisper.server

# 1. Build + install this snap
./dev/prepare.sh && snapcraft pack
sudo snap install --dangerous ./myna_*.snap

# 2. Connect the two manual interfaces
sudo snap connect myna:pipewire                          # mic capture (snapd gates it)
sudo snap connect myna:backend myna-whisper:ubustt-socket     # the backend session socket

# 3. Run the daemon (leave it running; autostart is a known gap below)
myna

# 4. Focus a text field, tap the key the portal bound, speak, tap again →
#    transcript injected.
```

That's it. If step 4 misbehaves, jump to **Troubleshooting**.

No activation, indicator or preedit flags: packaged, `myna` uses the
GlobalShortcuts portal, always serves `org.myna.Dictation`, and turns
streaming preedit on only where the tier gate says this machine streams. See
**Activation** for forcing any of them.

The `backend` plug is a writable content share of the backend snap's
`$SNAP_COMMON/run` (T14c): after connecting, the session socket appears at
`/var/snap/myna/current/backend/run/ubustt.sock`. One backend at a time
(whisper / nemotron / qwen provide the same slot; multi-backend selection
is T48). The backend daemon must have run at least once for the socket to
exist (`sudo snap start myna-whisper.server`).

## Activation

Everything is **press-to-toggle**: tap the key to start, tap again to stop.
Two trigger transports, and the daemon picks between them itself - the
portal only serves apps the compositor can identify, so `$SNAP` being set
*is* the availability test:

- **GlobalShortcuts portal (default here, because this is a snap)** — the
  sandboxed-native trigger. On xdg-desktop-portal-gnome 51 (GNOME Shell
  51.alpha) the bind is auto-accepted: no sheet, the grab registers at
  daemon start (verified 2026-08-18). Older backends may show a bind sheet
  once per start - portal v1 has no persist/restore token, and ashpd 0.13
  doesn't expose v2's. `myna --hold` switches it to hold-to-talk.
- **Control socket** (`myna --control`) — for a desktop with no working
  GlobalShortcuts backend. `myna` listens for pokes; `myna.toggle` sends
  one. Bind a custom shortcut to `/snap/bin/myna.toggle`
  (`myna.install-shortcut '<Super>t'` does it for GNOME).

`myna --stdin` drives from the terminal (debug; injects back into the
terminal). The three activation flags are mutually exclusive.

**Indicator**: `org.myna.Dictation` is always served for the myna-shell
GNOME extension, falling back to desktop notifications by itself when the
session bus is unreachable - so there is no flag to set. `myna --no-dbus`
forces the notification path for debugging. The experimental GTK overlay is
`--overlay`.

**Preedit**: in-field unstable hypotheses are on exactly when this machine
resolves to streaming (your persisted `streaming_mode` through the RTF tier
gate - see `docs/streaming-mode-settings.md`) *and* the injector has a real
preedit region. `myna --preedit` / `myna --no-preedit` force it either way.

**Env knobs**: `MYNA_BACKEND_SOCKET`, `MYNA_LANGUAGE`. (`MYNA_ACTIVATION` is
gone - use `--portal` / `--control` / `--stdin`.)

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
  and make sure the backend daemon has run (`snap logs myna-whisper.server`).
- **`myna.toggle` can't reach the daemon** — `myna` isn't running, or it's
  running in the default portal activation; `myna.toggle` needs
  `myna --control`.
- **Nothing is injected, state shows `error`** — read the reason:
  `gdbus call --session --dest org.myna.Dictation \
    --object-path /org/myna/Dictation \
    --method org.freedesktop.DBus.Properties.Get org.myna.Dictation ErrorMessage`
  (a *capture_failed* usually means `myna:pipewire` isn't connected).
- **A press "does nothing" - the session starts and dies silently** - the
  daemon serves `org.myna.Dictation` by default, so ALL feedback (including
  errors) goes to its properties; without the myna-shell extension nothing
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
  myna --portal                 # then press your key
  # org.freedesktop.portal.GlobalShortcuts Activated should appear on press;
  # nothing = the grab never registered (portal side)
  ```
