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

# 1. `myna` is a user daemon, and snapd gates those behind an experimental
#    flag unless the snap-id is allowlisted. Without this the INSTALL fails.
sudo snap set system experimental.user-daemons=true

# 2. Build + install this snap
./dev/prepare.sh && snapcraft pack
sudo snap install --dangerous ./myna_*.snap

# 3. Connect the two manual interfaces
sudo snap connect myna:pipewire                          # mic capture (snapd gates it)
sudo snap connect myna:backend myna-whisper:ubustt-socket     # the backend session socket

# 4. Focus a text field, tap the key, speak, tap again →
#    transcript injected.
```

The daemon is already running: installing enabled and started it, and it comes
back at every login. Nothing to launch - `snap services myna` should read
`enabled / active`. The very first start raises the portal's shortcut sheet
once; accept it and pick a key. If step 4 misbehaves, jump to
**Troubleshooting**.

No activation, indicator or preedit flags: packaged, `myna` uses the
GlobalShortcuts portal, always serves `com.canonical.Myna.Dictation`, and turns
streaming preedit on only where the tier gate says this machine streams. See
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
compositor, no PipeWire, no IBus, no portal. (`graphical-session.target`
ordering for `desktop`-plugging user daemons was added in snapd 2.74 and
reverted in 2.74.1, LP #2141607 - there is no knob for it.) So the daemon
treats all four as things that come and go rather than as preconditions:

- activation is bound with backoff and **re-bound** if the portal restarts;
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
  startup. What it does not have is a portal, so it re-checks for one forever.

So there is no guard, because there is nothing worth guarding against and
nothing sound to guard *with*: at PAM login "no compositor yet" and "no
compositor ever" are the same observation, which is exactly why snapd's own
`graphical-session.target` ordering for `desktop`-plugging user daemons was
reverted in 2.74.1 (LP #2141607). A daemon that guessed would be dead in the
normal case it guessed wrong about.

**Starting early is allowed; *waking* the desktop's services is not (decided
2026-08-26).** Ordering was the wrong lever, but starting first still has a
cost, and it has to be paid inside the daemon: every D-Bus call is
auto-starting. Binding activation before the compositor had exported
`XDG_CURRENT_DESKTOP` launched `xdg-desktop-portal` *itself*, and a portal
started in that window resolves its backends against an empty desktop -
`gtk.portal` as a last-resort fallback for every interface, never
`gnome.portal`, which is the only implementation of GlobalShortcuts. That map
is cached for the life of the session, so the hotkey stays dead until
`systemctl --user restart xdg-desktop-portal`, and so does every *other* app's
file chooser. Observed on a GNOME 49 Wayland login: the daemon started at
45.55 s, activated the portal at 45.83 s, and gnome-session only began at
45.89 s.

So the bind first asks whether anything already owns
`org.freedesktop.portal.Desktop` and reports the ordinary unavailable failure
if not (`shortcut/portal.rs::portal_is_up`), letting the existing backoff wait
for a portal rather than conjuring one. Whoever starts it in a real session
does so with the right environment. The rule generalises: a daemon that runs
before the desktop may *join* the desktop's services and must never summon
them.

**A missing desktop is waited on, never polled for.** "No portal yet" is the
one bind failure with its own disposition (`BindFailure::NotYet`), because it
is the only one that is not a failure at all - nothing was asked of anyone, and
the answer flips exactly once, when the desktop arrives. The daemon subscribes
to `NameOwnerChanged` for the portal's name and parks
(`portal::await_portal`), so it is asleep between login and the compositor, and
asleep forever on a machine where the compositor is never coming. The 30 s
`ABSENT_RECHECK` is a safety net for a missed notification, not the mechanism.

The alternative was polling, and it was measured before being rejected: at one
`NameHasOwner` a second it cost **73 ms of CPU per minute** with the portal
masked so nothing could start it, which is ~105 s of CPU a day in a lingering
headless account, forever, for a daemon with nothing to bind to. Waiting on the
event costs a signal match on a connection the daemon already holds:
**2 ms of CPU per minute** in the same conditions, which is the
`ABSENT_RECHECK` net firing twice and little else, and it binds *faster*: 55 ms
from the portal taking its bus name to `activation bound`, against 0.64 s when
polling.

Note what this is *not*: it is not detecting whether a desktop exists. That
cannot be done soundly, which is why there is no guard - and the portal bug
above is the proof rather than the theory. At 45.83 s into a real GNOME login,
in a session that was working perfectly well, `XDG_CURRENT_DESKTOP` was not yet
in the user manager's environment. Any check for "is there a desktop here"
would have answered no, on a laptop that was two seconds from a full GNOME
session. Waiting costs nothing and needs no such answer, so there is nothing
left to detect.

One consequence worth stating: waiting proves nothing about how the backend
behaves, so it does not spend the backoff. A daemon that waited out a long
login still meets the portal's first real failure at the bottom of the
1/2/4...30 s ladder rather than at its ceiling.

The one real cost was the retry log - ~2,900 identical lines a day on a machine
with no compositor. A bind failure is now reported once at the operational
tier, again only when the reason changes, and the repeats go to `MYNA_DEBUG`;
the current reason is continuously readable on
`com.canonical.Myna.Dictation.StatusMessage`, which is the surface for the
current publisher-owned user-facing status.

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
  sandboxed-native trigger. On xdg-desktop-portal-gnome 51~alpha the **first**
  bind raises a shortcut sheet; accept it and pick a key. It is remembered
  after that - later daemon starts and portal restarts re-bind silently in
  ~50ms (measured 2026-08-25, correcting an earlier "auto-accepted, no sheet"
  note from 2026-08-18). An unanswered sheet leaves the bind pending
  *indefinitely*: the portal resolves it on a `Response` signal, so no D-Bus
  call timeout applies. The daemon bounds that at 120s and retries.
  `myna --hold` switches it to hold-to-talk.

  To change the key afterwards: **Settings → Keyboard**, where it is listed
  under myna. Do *not* bind a GNOME custom shortcut to `myna.toggle` for this -
  `gsd-media-keys` serves custom keybindings and portal global shortcuts alike,
  so a custom binding on the same accel shadows the portal's own and the key
  stops working. `myna.install-shortcut` refuses under portal activation for
  exactly this reason.
- **Control socket** (`myna --control`) — for a desktop with no working
  GlobalShortcuts backend. `myna` listens for pokes; `myna.toggle` sends
  one. Bind a custom shortcut to `/snap/bin/myna.toggle`
  (`myna.install-shortcut '<Super>t'` does it for GNOME). Both commands are
  control-activation only; under the default they are inert and say so.

`myna --stdin` drives from the terminal (debug; injects back into the
terminal). The three activation flags are mutually exclusive.

**Indicator**: `com.canonical.Myna.Dictation` is always served for the myna-shell
GNOME extension, falling back to desktop notifications by itself when the
session bus is unreachable - so there is no flag to set. `myna --no-dbus`
forces the notification path for debugging. The experimental GTK `--overlay`
was removed (T150).

**Preedit**: in-field unstable hypotheses are on exactly when this machine
resolves to streaming (your persisted `streaming_mode` through the RTF tier
gate - see `docs/streaming-mode-settings.md`) *and* the injector has a real
preedit region. `myna --preedit` / `myna --no-preedit` force it either way.

**Env knobs**: `MYNA_BACKEND_SOCKET`, `MYNA_LANGUAGE`.
(`MYNA_ACTIVATION` is gone - use `--portal` / `--control` / `--stdin`.)

## Apps

| app | what |
|---|---|
| `myna` | the dictation daemon - a user service, so no `/snap/bin` entry |
| `myna.status` | what state dictation is in, and why - start here |
| `myna.toggle` | poke the daemon's control socket (start/stop). **Control activation only** - the default (portal) daemon has no control socket |
| `myna.install-shortcut` | bind a GNOME custom shortcut → `myna.toggle` (dconf). **Control activation only** - refuses under portal, where it would shadow the portal's own binding |
| `myna.testbed` | the `myna-dictate` testbed CLI (`--list-devices`, `--clip`, `--dialect`, …) |

### `myna.status`

The four planes that answer "why is it doing that" used to be four places: the
persisted values in `gsettings`, what they resolved to in a journal line
printed once at startup, the backend socket nowhere at all, and the live state
on the bus. This prints the composition, including *which* plane won each value
- because "I set that and nothing happened" is the question being asked.

```
settings   com.canonical.Myna.Dictation (schema installed)
  activation      (unset)      -> Portal (packaged)      [built-in]
  language        (unset)      -> (backend default)      [built-in]
  hotkey          (unset)      -> (portal default)       [built-in]
  streaming-mode  auto         -> preedit false          [gsettings]
                  streaming-mode Auto resolves to Batch on tier x86_64-cpu-generic

backend
  configured      /var/snap/myna/current/backend/*/ubustt.sock
  resolves to     /var/snap/myna/current/backend/run/ubustt.sock

daemon     com.canonical.Myna.Dictation
  state           idle
  error           (none)
```

Run it confined (`myna.status`, not a local build): `$SNAP` decides activation,
and the backend share is a bind mount that exists only inside the snap, so an
unpackaged `--status` reports a healthy packaged daemon's backend as
unreachable. It says so when it notices.

## Verify (confined, end to end)

```shell
# 1. testbed round-trip through the content-shared socket
myna.testbed --socket /var/snap/myna/current/backend/run/ubustt.sock \
    --language en --clip ~/path/to/clip.wav

# 2. device enumeration over the confined PipeWire socket
myna.testbed --list-devices

# 3. daemon + bus: com.canonical.Myna.Dictation is owned while `myna` runs
gdbus introspect --session --dest com.canonical.Myna.Dictation \
    --object-path /com/canonical/Myna/Dictation
```

## Troubleshooting

- **A press reports "no backend is connected"** - connect the backend plug
  (step 2) and make sure the backend daemon has run (`snap logs
  myna-whisper.server`). The daemon does not need restarting afterwards: the
  socket is re-resolved at every press.
- **The hotkey does nothing right after login** - read `StatusMessage` (below),
  or `journalctl --user -u snap.myna.myna`. `dictation hotkey unavailable: …`
  means activation is not bound; it clears itself once it is. Two retry
  speeds, by cause: the portal not being up yet is retried at 1s doubling to
  30s, while a refused or unanswered shortcut sheet waits 5 minutes - retrying
  that one fast would just re-raise the dialog. Dismissed the sheet by
  accident? `sudo snap restart myna` brings it straight back.
- **`myna.toggle` can't reach the daemon** — `myna` isn't running, or it's
  running in the default portal activation; `myna.toggle` needs
  `myna --control`.
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
  lines - where the trail stops is the culprit). A
  `pipewire: mod.client-node: detected old client version 5` journal line
  at press time is benign: it's the snap-staged (older) libpipewire
  connecting, and since capture starts only per press, it proves the
  hotkey fired. Classic silent-death cause: `--socket` /
  `MYNA_BACKEND_SOCKET` pointing at a backend snap's
  `/var/snap/<snap>/common/run/...` directly - confinement denies it (the
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
| `desktop` | GlobalShortcuts portal + desktop notifications |
| `desktop-legacy` | the IBus daemon's private socket (text injection) |
| `gsettings` | the client settings store (`com.canonical.Myna.Dictation`) and the dconf write for `myna.install-shortcut` |
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

Two planes, and the order between them is the whole design:

```shell
gsettings set com.canonical.Myna.Dictation streaming-mode streaming   # per user
sudo snap set myna language=fr                              # per machine
```

**A flag beats the user's GSettings value, which beats `snap set`, which beats
the built-in.** snapd's configuration is per *snap*, not per user, so it can
only ever be a default: an admin (or an image build) presetting a language must
not overrule an account that chose its own.

| key | gsettings | snap set | effect |
|---|---|---|---|
| `streaming-mode` | `auto` \| `streaming` \| `batch` | - | emission mode, and with it in-field partials |
| `language` | any short code | `language=fr` | session language hint |
| `activation` | `auto` \| `portal` \| `control` | `activation=control` | how a press reaches the daemon |
| `hotkey` | `'<Super>d'` | `hotkey='<Super>d'` | the accelerator offered to the portal |

Settings are read once, at start:

```shell
sudo snap restart myna
journalctl --user -u snap.myna.myna | grep settings:
#  settings: streaming-mode Auto resolves to Streaming on tier x86_64-cpu-generic
#  settings: activation Portal, language (backend default), hotkey (portal default)
```

`snap set` validates: a bad value fails the `snap set` itself rather than
landing a setting the daemon would have to ignore.

The snap carries its own compiled copy of the schema, so `gsettings` works
against it with nothing installed on the host; a host *tool* needs the schema
installed (`make install-schema`, until the extension deb carries it). Reads and
writes cross confinement because `XDG_CONFIG_HOME` points at
`$SNAP_REAL_HOME/.config`: libdconf derives the database path from it, and
snapd's `$HOME` would otherwise send the daemon to a database nothing writes.

`auto` gates on a measured RTF baseline, and the snap ships none - so `auto`
means batch today. That is the safe end of the failure, and the reason is
recorded in T77: the tier key is the architecture alone, so shipping one
machine's measurement would promise streaming to every machine of that arch.

## Known gaps (tracked)

- `experimental.user-daemons` is a manual step until the snap-id is
  allowlisted upstream (see **The daemon**).
- No RTF baseline is installed, so `streaming-mode=auto` gates to batch
  everywhere. The tier key is architecture-only, which makes a shipped
  measurement a promise about machines nobody measured (T77).
- No `default-provider` on the `backend` plug, so installing a backend is a
  separate step rather than an install prerequisite.
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
