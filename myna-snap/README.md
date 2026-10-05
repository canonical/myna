# myna-snap

The Myna dictation **client** snap - ships the Rust orchestrator
(`client/myna-desktop`, the push-to-talk app, plus the `myna-testbed`
CLI). Feature `005-myna-orchestrator-snap`
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

# 1. `myna` is a user daemon, and snapd gates those behind an experimental
#    flag unless the snap-id is allowlisted. Without this the INSTALL fails.
sudo snap set system experimental.user-daemons=true

# 2. Build + install this snap. The version comes from git, as snapcraft's
#    `version: git` would give it, less the tag's v: 0+git.<sha>, -dirty for
#    uncommitted changes (dev/version.sh).
./dev/prepare.sh && snapcraft pack
sudo snap install --dangerous ./myna_*.snap

# 3. Connect the two manual interfaces
sudo snap connect myna:pipewire                          # mic capture (snapd gates it)
sudo snap connect myna:backend myna-whisper:provider         # the backend session socket
#    Upgrading from a build with the old `ubustt-socket` slot? snapd keeps that
#    connection across the refresh: `sudo snap disconnect myna:backend` first.

# 4. Focus a text field, tap the key, speak, tap again →
#    transcript injected.
```

The daemon is already running: installing enabled and started it, and it comes
back at every login. Nothing to launch - `snap services myna` should read
`enabled / active`. Myna Settings sets up the key; without it, run
`myna.install-shortcut '<Super>j'`. If step 4 misbehaves, jump to
**Troubleshooting**.

No activation, indicator or preedit flags: `myna` listens on its control
socket, always serves `com.canonical.Myna.Dictation`, and turns
streaming preedit on whenever the transcription mode in force is `streaming`:
your `streaming-mode` if you set one, else the backend's own mode. See
**Activation** for forcing any of them.

## The daemon

`myna` is a **per-user systemd service** (`daemon: simple` +
`daemon-scope: user`). snapd generates
`/etc/systemd/user/snap.myna.myna.service` with `WantedBy=default.target` and
`Restart=on-failure`, so it starts at login for every logged-in user and is
restarted if it dies.

```shell
snap services myna                       # enabled / active
sudo snap restart myna                   # stop/start also work
journalctl --user -u snap.myna.myna -f   # `snap logs myna` needs sudo for user units
```

**Install precondition.** snapd rejects the *install* of any snap declaring a
user daemon unless `experimental.user-daemons` is set, or the snap-id is on the
hardcoded allowlist in `overlord/snapstate/snapstate.go:82`. A `--dangerous`
install has no snap-id at all, so dev and CI always need the flag (step 1
above); the store path is a snapd PR adding this snap's id, which needs the
name registered and uploaded first.

**It starts before the desktop does.** `default.target` is PAM login: no
compositor, no PipeWire, no IBus. (`graphical-session.target`
ordering for `desktop`-plugging user daemons was added in snapd 2.74 and
reverted in 2.74.1, LP #2141607 - there is no knob for it.) So the daemon
treats all three as things that come and go rather than as preconditions:

- the control socket is bound with backoff, since `$XDG_RUNTIME_DIR` may not
  exist yet;
- IBus is connected at the first press that needs it, and reconnected after an
  `ibus restart`;
- the backend socket is re-resolved at every press, so `snap connect` and
  `snap refresh myna-whisper` need no restart here;
- a second `myna` finds the bus name taken and exits 0.

Nothing in that list is a reason to exit, which matters more than it sounds:
the generated unit has no `StartLimitBurst` override, so five exits in ten
seconds would leave the unit permanently `failed`.

**It starts in every user manager, and that is left alone (decided
2026-08-26).** `WantedBy=default.target` is per *user*, not per session, so the
unit is reached by any `systemd --user` instance: a graphical login, an SSH
login, a lingering headless account, the gdm greeter. Measured rather than
argued:

- **the greeter never runs it.** gdm's home is `/var/lib/gdm`, outside `/home`,
  and `snap run` refuses to start there ("home directories outside of /home
  needs configuration"). The unit fails five times, hits systemd's restart
  limit and stops - ten journal lines per greeter start, and no daemon.
- **a headless account runs it healthily**: `active`, `NRestarts=0`, 4.6 MB
  cgroup memory and 188 ms of CPU over its first half-minute, all of that
  startup.

So there is no guard, because there is nothing worth guarding against and
nothing sound to guard *with*: at PAM login "no compositor yet" and "no
compositor ever" are the same observation, which is exactly why snapd's own
`graphical-session.target` ordering for `desktop`-plugging user daemons was
reverted in 2.74.1 (LP #2141607). A daemon that guessed would be dead in the
normal case it guessed wrong about.

A daemon that runs before the desktop may *join* the desktop's services and
must never summon them: every D-Bus call is auto-starting, and a portal started
before the compositor exports `XDG_CURRENT_DESKTOP` resolves its backends
against an empty desktop for the whole session.

**What `snap refresh myna` does.** It stops and restarts the unit in every user
manager, on the new revision (`refresh-mode: restart`, stated explicitly in
`snapcraft.yaml`). A refresh landing mid-utterance costs that utterance;
activation rebinds by itself. Snapd's refresh-app-awareness does not apply -
it holds back refreshes for running *apps*, and a daemon is never one, so
there is nothing to opt into.

There is no `/snap/bin/myna` - snapd skips wrappers for service apps
(`wrappers/binaries.go:218`). To drive it by hand:
`sudo snap stop myna && snap run myna --stdin`.

## The backend socket

The `backend` plug consumes the `inference-provider` content interface: a
backend snap's `provider` slot shares its `$SNAP_COMMON/share/provider`, which
lands at `/var/snap/myna/current/backend/provider` (`provider-2`, … for further
connections). Each share holds a `provider.env` naming the snap and its
`UNIX_SOCKET`, here `myna.sock`. The daemon reads them at every press: a share
without `provider.env` is ignored, a provider offering no Unix socket (a
TCP-only LLM snap) or whose server is not running is named in the "not
connected" message, and more than one usable provider is an error rather than
a guess (multi-backend selection is T48). The backend server must be running
for the socket to exist (`sudo snap start myna-whisper.server`).

## Activation

Everything is **press-to-toggle**: tap the key to start, tap again to stop.
The key is a GNOME custom shortcut, which Myna Settings writes, calling the
daemon's `com.canonical.Myna.Dictation.Toggle` over D-Bus. `myna` also listens
on a control socket that `myna.toggle` pokes, slower by `snap run`'s
startup; `myna.install-shortcut '<Super>t'` binds that without Myna
Settings.

The GlobalShortcuts portal was the packaged default until 2026-10 and is gone:
stacked and duplicate consent dialogs, consent and retry races, an app id that
drifted between `myna_myna` and `.`, grants that survived `snap remove
--purge`, grabs that needed a re-login, and no GlobalShortcuts at all on
Noble. Hold-to-talk, the one thing only the portal offered, is not planned.

`myna --stdin` drives from the terminal (debug; injects back into the
terminal). `--control` and `--stdin` are mutually exclusive.

**Indicator**: `com.canonical.Myna.Dictation` is always served for the myna-shell
GNOME extension, falling back to desktop notifications by itself when the
session bus is unreachable - so there is no flag to set. `myna --no-dbus`
forces the notification path for debugging. The experimental GTK `--overlay`
was removed (T150).

**Preedit**: in-field unstable hypotheses are on exactly when the mode in force
is `streaming` - your `streaming-mode` if set, else whether the backend streams
(see `client/.kb/runtime-settings.md`) - *and* the injector has a real preedit
region. `myna --preedit` / `myna --no-preedit` force it either way.

**Env knobs**: `MYNA_BACKEND_SOCKET`, `MYNA_LANGUAGE`.
(`MYNA_ACTIVATION` is gone - use `--control` / `--stdin`.)

## Apps

| app | what |
|---|---|
| `myna` | the dictation daemon - a user service, so no `/snap/bin` entry |
| `myna.status` | what state dictation is in, and why - start here |
| `myna.config` | query/change the persisted settings: glib's gsettings over the snap's keyfile store. Bare, it lists every key; `set`/`get`/`reset` take bare values (`myna.config set language fr`) |
| `myna.toggle` | poke the daemon's control socket (start/stop) |
| `myna.install-shortcut` | bind a GNOME custom shortcut → `myna.toggle` (dconf); the one app with the `gsettings` plug |
| `myna.testbed` | the `myna-testbed` CLI (`--list-devices`, `--clip`, `--dialect`, …) |

### `myna.status`

The four planes that answer "why is it doing that" used to be four places: the
persisted values in the settings store, what they resolved to in a journal
line printed once at startup, the backend socket nowhere at all, and the live
state on the bus. This prints the composition, including *which* plane won each
value - flag, settings, backend or built-in - because "I set that and nothing happened"
is the question being asked.

```
settings   com.canonical.Myna.Dictation (schema installed)
  activation      (flag only)  -> Control                  [built-in]
  language        (unset)      -> (backend default)        [built-in]
  streaming-mode  (unset)      -> batch, preedit false     [backend]

backend
  configured      /var/snap/myna/current/backend/*/provider.env
  provider        myna-whisper
  resolves to     /var/snap/myna/current/backend/provider/myna.sock
  model           tiny
  streams         false

daemon     com.canonical.Myna.Dictation
  state           idle
  error           (none)
```

Run it confined (`myna.status`, not a local build): the backend share is a bind mount that exists only inside the snap, so an
unpackaged `--status` reports a healthy packaged daemon's backend as
unreachable. It says so when it notices.

## Verify (confined, end to end)

```shell
# 1. testbed round-trip through the content-shared socket (found the way the
#    daemon finds it: --backend-dir $SNAP_DATA/backend, passed by the wrapper)
myna.testbed --language en --clip ~/path/to/clip.wav

# 2. device enumeration over the confined PipeWire socket
myna.testbed --list-devices

# 3. daemon + bus: com.canonical.Myna.Dictation is owned while `myna` runs
gdbus introspect --session --dest com.canonical.Myna.Dictation \
    --object-path /com/canonical/Myna/Dictation
```

## Troubleshooting

- **A press reports "Model not connected"** - connect the backend plug
  (step 2) and make sure the backend daemon has run (`snap logs
  myna-whisper.server`). The daemon does not need restarting afterwards: the
  socket is re-resolved at every press.
- **The hotkey does nothing** - check the daemon answers the call the
  shortcut makes: `gdbus call --session --dest com.canonical.Myna.Dictation
  --object-path /com/canonical/Myna/Dictation --method
  com.canonical.Myna.Dictation.Toggle`. A `myna.toggle` shortcut needs the
  control socket, whose bind is retried at 1s doubling to 30s
  (`journalctl --user -u snap.myna.myna`).
- **`myna.toggle` can't reach the daemon** — `myna` isn't running.
- **Nothing is injected, state shows `error`** — read the status:
  `gdbus call --session --dest com.canonical.Myna.Dictation \
    --object-path /com/canonical/Myna/Dictation \
    --method org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation StatusMessage`
  (a *capture_failed* usually means `myna:pipewire` isn't connected).
- **A press "does nothing" - the session starts and dies silently** - the
  daemon serves `com.canonical.Myna.Dictation` by default, so ALL feedback (including
  errors) goes to its properties; without the myna-shell extension nothing
  renders it (notifications are only the fallback when the bus can't be
  owned). Critical session errors are always printed to the daemon's
  stderr, so run `myna` from a terminal and read them there. For the full
  stage-by-stage trace add `MYNA_DEBUG=1` (`ctrl`/`capture`/`ws`/`inject`
  lines - where the trail stops is the culprit). That tier prints the
  transcript text itself, and as a systemd service the daemon's stderr is
  the journal, so it persists there until the journal rotates: set it for a
  repro, unset it after. A
  `pipewire: mod.client-node: detected old client version 5` journal line
  at press time is benign: it's the snap-staged (older) libpipewire
  connecting, and since capture starts only per press, it proves the
  hotkey fired. Classic silent-death cause: `--socket` /
  `MYNA_BACKEND_SOCKET` pointing at a backend snap's
  `/var/snap/<snap>/common/share/provider/...` directly - confinement denies it (the
  `backend` content share exists precisely for this); the denial shows in
  `sudo journalctl -k`. Live state without restarting: read the
  `State`/`StatusMessage` properties as above.
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
| `desktop` | desktop notifications |
| `desktop-legacy` | the IBus daemon's private socket (text injection) |
| `gsettings` | the dconf write for `myna.install-shortcut` - the only app with it |
| `network-bind` | seccomp `bind(2)` for the control socket - no outbound reach, and no other interface grants it |
| `wayland`, `x11` | the GTK indicator window |
| `backend` (content) | the backend session socket |
| slot `com.canonical.Myna.Dictation` (dbus) | the indicator publisher (state + level only) |

The IBus injector finds the daemon's address file under your *real* home
even though snapd redirects `$HOME` (feature-005 discovery fix); the
control socket lives under the snap-scoped `$XDG_RUNTIME_DIR`.

**Confinement note (indicator bus):** `com.canonical.Myna.Dictation` is properties-only
by design. snapd's `dbus` slot AppArmor policy denies broadcasting *custom*
signals to unconfined subscribers (and can't be safely widened — AppArmor
dbus rules can't discriminate message types), but it does allow
`org.freedesktop.DBus.Properties` sends on the service's own path, which is
exactly the shape of a `PropertiesChanged` broadcast. State + level updates
are therefore pushed with standard `PropertiesChanged`; the myna-shell
extension subscribes and gets the fast push path confined or not — no
polling (contract `specs/004-gnome-shell-indicator/contracts/dbus-interface.md`
§Confinement).

## Settings

One plane: the per-user settings store, a plaintext keyfile only this snap
can see:

```
~/snap/myna/common/.config/glib-2.0/settings/keyfile
```

(`common`, not the per-revision `current`: `snap revert` must not revert
settings. `snap remove` snapshots it into the automatic snapshot;
`snap remove --purge` deletes it.)

Read and write it with `myna.config` - glib's own gsettings over that store,
with the schema id filled in. Bare, it lists every key; `set`/`get`/`reset`
take bare values. Changes reach the running daemon with no restart (the
backend's file monitor); `streaming-mode` and `language` apply live.
"Is this key set?" is `cat` of the file - an absent
key reads the schema default, so the file only ever holds what was set.

```shell
myna.config                                    # every key, set or default
myna.config set streaming-mode batch
myna.config reset streaming-mode
```

**A flag beats the user's settings value, which beats the built-in.**

| key | values | effect |
|---|---|---|
| `streaming-mode` | `streaming` \| `batch` | emission mode, and with it in-field partials; unset follows the backend |
| `language` | any short code | session language hint |

The daemon logs what it resolved at every start:

```shell
journalctl --user -u snap.myna.myna | grep settings:
#  settings: activation Control, language (backend default), preedit true (from streaming-mode Streaming, the schema default while the backend's is unknown)
#  settings: backend streams -> false
#  settings: preedit -> false (from streaming-mode Batch, the backend's default)
```

Notes:

- The store moved here from the host's dconf database (pre-2026-09). Old
  values are not migrated; carry one over by hand if you need it (dconf's
  dump is already keyfile syntax):
  `dconf dump /com/canonical/myna/ > ~/snap/myna/common/.config/glib-2.0/settings/keyfile`
- `snap set myna ...` is not a thing. snapd accepts and silently stores
  arbitrary keys; nothing in this snap reads them.
- Deleting the keyfile does not reset a running daemon (glib's backend
  ignores file-deleted events); reset keys, don't remove the file.

## Known gaps (tracked)

- `experimental.user-daemons` is a manual step until the snap-id is
  allowlisted upstream (see **The daemon**).
- No `default-provider` on the `backend` plug, so installing a backend is a
  separate step rather than an install prerequisite.
- Socket access control is "an admin connected the plug" — identity/polkit
  is T17.
- Store name `myna` is unregistered as of 2026-07-22; register before any
  store upload.
