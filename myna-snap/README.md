# myna-snap

The UbuSTT dictation **client** snap — ships the Rust orchestrator
(`client/myna-desktop`, the push-to-talk app, plus the `myna-dictate`
testbed CLI). Feature `005-myna-orchestrator-snap`
(`specs/005-myna-orchestrator-snap/`); plan task T57.

This snap is the mirror image of the inference snaps: **it** owns the
microphone, the hotkey, and text injection (audio-push invariant); the
backend snaps only receive PCM on a socket. It deliberately has **no
`network` plug** — every boundary is a Unix socket or the session bus.

## Build

```shell
./dev/prepare.sh   # stage client/ into the project (craft-parts rule)
snapcraft pack
```

## Install

```shell
sudo snap install --dangerous ./myna_*.snap

sudo snap connect myna:pipewire                          # mic capture (snapd gates it)
sudo snap connect myna:backend whisper:ubustt-socket     # the backend session socket
```

The `backend` plug is a writable content share of the backend snap's
`$SNAP_COMMON/run` (T14c): after connecting, the session socket appears at
`/var/snap/myna/current/backend/run/ubustt.sock`. One backend at a time
(whisper / nemotron / qwen provide the same slot; multi-backend selection
is T48). The backend daemon must have run at least once for the socket to
exist (`sudo snap start whisper.server`).

## Run

```shell
myna            # the daemon: portal hotkey + org.myna.Dictation publisher
```

- **Activation** defaults to the **control socket** (`myna.toggle`) — bind a
  key with `myna.install-shortcut` (Super+D → `/snap/bin/myna.toggle`, via
  dconf) or any GNOME custom shortcut to that command. The **portal**
  (`MYNA_ACTIVATION=portal myna`) is opt-in for now: the GlobalShortcuts
  portal here is v1 — it re-prompts with the bind sheet on every daemon
  start (persist/restore tokens are v2, unexposed by ashpd 0.13) and the
  grab did not register reliably on GNOME 50 (under investigation — see the
  debug recipe below). `MYNA_ACTIVATION=stdin myna` for terminal debugging.
- **Indicator**: `--dbus` mode is on by default in the launcher, serving
  `org.myna.Dictation` for the myna-shell GNOME extension; desktop
  notifications are the fallback. The experimental GTK overlay is available
  (`MYNA_ACTIVATION=portal myna --overlay`).
- **Env knobs**: `MYNA_BACKEND_SOCKET`, `MYNA_ACTIVATION`, `MYNA_LANGUAGE`.

## Apps

| app | what |
|---|---|
| `myna` | the dictation daemon (launcher around `myna-desktop`) |
| `myna.toggle` | poke the daemon's control socket (control activation mode) |
| `myna.install-shortcut` | bind Super+D → `myna.toggle` via dconf |
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

> **Read the bus from a *host* shell.** snapd's `dbus` slot policy only
> admits `label=unconfined` peers, so calling `org.myna.Dictation` from a
> *confined* context (a `snap run --shell`, a Workshop/LXD/toolbox container
> with the session bus forwarded) fails with `Access denied`. The GNOME
> Shell extension is in-compositor (unconfined) and unaffected.

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
control socket lives under `$XDG_RUNTIME_DIR/snap.myna/` as AppArmor
requires.

## Known gaps (tracked)

- No autostart on login yet (snapd user daemons are still experimental);
  start `myna` from a terminal or Startup Applications.
- Socket access control is "an admin connected the plug" — identity/polkit
  is T17.
- Store name `myna` is unregistered as of 2026-07-22; register before any
  store upload.
- **Signal reachability:** a confined snap can't broadcast custom
  session-bus signals to unconfined subscribers (snapd's dbus slot admits
  unconfined peers for method calls/replies only — AppArmor denies the
  `StateChanged` broadcast). The myna-shell extension therefore *polls*
  `GetAll` (fast during a session, slow when idle) as well as subscribing;
  signals remain the fast path for an unsnapped daemon. Candidate upstream
  snapd fix: allow signal sends on the slot's interface to unconfined peers.
- **Portal hotkey (v1):** re-prompts every daemon start; grab unreliable on
  GNOME 50. Diagnose with:
  ```shell
  gdbus monitor --session --dest org.freedesktop.portal.Desktop &
  MYNA_ACTIVATION=portal myna   # confirm the sheet, then hold your key
  # org.freedesktop.portal.GlobalShortcuts Activated should appear on press;
  # nothing = the grab never registered (portal side)
  ```
