# Onboarding

The settings application ships separately from the `myna` snap, so it can be
opened on a machine where dictation is not installed at all. When that is the
case it opens a three-step wizard instead of the settings window.

Every step leaves through one footer button, Next and on the last step Done:
a stock libadwaita button at its natural size, `suggested-action` while moving
on is the step's main action (`onboarding::forward_leads`) and plain
otherwise. The Figma mock draws Yaru.dart's green and outlined buttons; stock
Adwaita was chosen over them. The window opens at the
design's 800x600, and every step's header is flat and untitled; each step after
the first carries a back arrow to the step before it. Done closes Myna
Settings, as the design says: the wizard and, when it was opened from the menu,
the settings window under it. It closes each window rather than quitting, so
their close handlers still stop what they run.
The welcome step loads the application icon straight from the application's
own resources, not by name through the icon theme: a stale icon cache that
still lists a deleted hicolor copy made the theme fail without falling back.

## What opens it

`myna_config::onboarding` assesses four components from the observations the
application already makes at startup (`snap list`, `snap connections` and
`snap interface content`), snapd's `/v2/system-info` read as the user, and
gnome-shell's `org.gnome.Shell.Extensions`:

| Component       | Required | Satisfied when                                          |
| --------------- | -------- | ------------------------------------------------------- |
| User daemons    | yes      | snapd's `experimental.user-daemons` is on               |
| Myna            | yes      | the `myna` snap is installed                            |
| Model           | yes      | discovery reports at least one backend                  |
| Shell extension | no       | gnome-shell runs the system copy of `myna-shell@canonical.com` |

The wizard opens when a required component is missing. The flag stays
required once Myna is installed: an installed Myna keeps running with the
flag unset, but snapd refuses its refreshes. "Model" is satisfied by
discovery rather than by a snap name: which snaps are backends is a property
of the socket interface they publish, not of their name. Every other row
waits for the flag, since snapd refuses Myna without it.

The extension is optional because dictation works without it: the daemon
falls back to desktop notifications. The myna-config deb, which this
application ships in, installs it to `/usr/share/gnome/gnome-shell/extensions`,
where it shadows Ubuntu's packaged copy on Stonking, so the wizard never
installs it and unavailable means a copy gnome-shell cannot run, not a
missing one. Only a system copy counts, not a development copy in `~/.local`.
Its state is one of:

- enabled: satisfied;
- disabled: gnome-shell lists the system copy but does not run it, and one
  `EnableExtension` call fixes it;
- installed after login: the copy is under a system data directory but
  gnome-shell, which scans them only at login, does not list it;
- shadowed: a system copy is on disk and so is a user copy of the same uuid
  under `~/.local/share/gnome-shell/extensions`. gnome-shell loads the user
  directory first and skips a uuid it already has, so the system copy never
  runs and a re-login does not help; removing the user copy does. Checked on
  disk, since gnome-shell keeps listing a user copy deleted after login;
- turned off: the user switched all extensions off in the Extensions app
  (gnome-shell's `UserExtensionsEnabled` false, `disable-user-extensions`
  true), which holds the system copy off and makes its `canChange` false. The
  row says so; the wizard does not flip that switch, which would turn every
  one of the user's extensions back on. On the Ubuntu session this does not
  arise where the session mode (`/usr/share/gnome-shell/modes/ubuntu.json`)
  lists myna-shell, as Stonking's does: a mode extension runs and stays
  changeable whatever that switch says. It does on a series or session whose
  mode does not list it (measured on Noble's gnome-shell 46 with a system
  copy: `state` 6, `canChange` false, `UserExtensionsEnabled` false);
- failed: gnome-shell ran the system copy and it errored (`state` 3), or
  reports a state this code does not know. The row says it failed to start;
- out of date: its `shell-version` lacks the running gnome-shell (`state` 4).
  The row says it does not work with this version of GNOME;
- locked: the administrator locked it (`canChange` false with extensions on:
  `enabled-extensions` is not writable). The row says so;
- unavailable: no system copy, or no gnome-shell answering on the session
  bus within 2 s. A shell that does not report `UserExtensionsEnabled` is
  taken as having extensions on.

gnome-shell sends `type` and `state` as doubles; `type` 1 is a system copy,
`state` 1 enabled, 2 and 6 disabled and never enabled. The transient 8
(activating) and 7 (deactivating) read as the state they are heading for, so
a re-read right after `EnableExtension` does not flash the row unavailable.
Enabling itself waits past 8 for 1, since an activating extension may still
error.

The settings window's main menu reopens the wizard (Set Up Dictation), modal
over the window. It refuses while a backend operation is in flight: the wizard
connects a backend and restarts the daemon, and the window's operation gate does
not cover it. Closing the wizard rediscovers, since it may have changed both.

## Installing

The component step titles itself "Install components" and lists every
component in its own row; nothing is left for a terminal. The flag gets a boxed
row of its own with a switch, "Let Myna run in the background" (its subtitle
names the snapd flag, `experimental.user-daemons`),
because it is a system setting rather than something to install. The other
three share one boxed list below it: Dictation app, Speech-to-text model and
Shell extension. The whole list is insensitive until the flag is on, since
snapd refuses Myna without it.

Each row ends in what the wizard can do about it (`onboarding::row_action`):

- Install, for a missing snap;
- Enable, for a system copy of the extension gnome-shell is not running;
- a check and "Installed" once it is in place;
- nothing, for an extension out of the wizard's reach. The row stays
  sensitive, since an insensitive row dims its subtitle past reading, and the
  subtitle says why: not on this system (dictation still works and
  shows its status in notifications), installed after login (log out and back
  in), or hidden by a copy in `~/.local/share/gnome-shell/extensions`.

The switch turns the flag on through snapd's REST API as the user
(`PUT /v2/snaps/system/conf`), and snapd raises polkit's prompt for
`io.snapcraft.snapd.manage-configuration` itself: no root code of ours, one
prompt. The prompt therefore shows snapd's wording ("access or modify snap
configuration"), not a Myna one. While snapd has not answered, the switch
shows on but not yet active, a spinner sits beside it and the subtitle reads
"Enabling…"; the row stops taking input but stays sensitive, as the settings
window's busy rows do. snapd answers only once the prompt is, 40 s for one
left open on Noble, so the write waits up to 10 min for that answer
(`SnapdTimeouts::authorization`) before following the change; interface
connects wait the same way. Dismissing the prompt puts the switch back
silently; a refusal or a failed change puts it back with a toast whose
Details open the report, which names the snapd request and its HTTP
status rather than a command. Success keeps the switch pending until a fresh read
shows the flag, then unlocks the list. A read that started before the write
is discarded rather than taken for the machine after it. The switch never
turns the flag off: activating it again springs back, since snapd refuses
Myna's refreshes without the flag.

Install asks snapd for the snap as the user
(`POST /v2/snaps/<name>`, `{"action":"install","channel":"latest/edge"}`),
the same install `snap install --edge` makes, and snapd raises polkit's prompt
for `io.snapcraft.snapd.manage` itself. That action is `auth_admin_keep` per
process, so installing the model within five minutes of the app asks nothing
more. snapd answers once the prompt is answered; the row then follows the
change `GET /v2/changes/<id>` once a second (`snap_install.rs`); ten failed
reads in a row end it, so snapd restarting mid-install is no failure. The button
gives way to a spinner and "Installing…", then "Installing 42%" once a
download announces its size: the bytes of every download task in the change
over what they announce or what the row expected to fetch, whichever is
more, never going down. The percentage shows only while a download runs:
mounting, hooks and services take 15 s or more after the app's download,
and "Installing 100%" there read as stuck (seen on Noble). The model's
component download runs inside the backend's own install change, fetched by
its install hook's engine choice, so the model row follows it to the end,
the percentage resuming where the snap's own download left it. A download
served from snapd's cache reports no bytes, so a cached install shows no
percentage.

Enable asks the user's own gnome-shell over the session bus
(`org.gnome.Shell.Extensions.EnableExtension`): no polkit, no prompt. The
call only adds the uuid to `enabled-extensions`; gnome-shell starts the
extension once that setting changes, so the row shows a spinner and
"Enabling…" until `GetExtensionInfo` reports it running, for up to 5 s.
A system copy gnome-shell already lists runs at once on X11 and Wayland
alike, so no re-login is asked for; the only re-login cases are the copy
installed after login and the shadowed one above, which offer no button.
gnome-shell refusing (`false`, an unknown uuid), the extension erroring
(gnome-shell's own `error` text) or not starting in time puts Enable back
with a toast whose Details name the D-Bus call. An extension with
`canChange` false is not offered: it is turned off or unavailable, as
above. Enabling it is the last missing piece on a machine with the rest
installed, so it moves the step on like an install. Enable runs beside a
snapd install, since it asks no prompt; only the snap rows wait for one
another.

A row snapd is still installing counts as missing (`onboarding::while_installing`)
whatever a read finds half-way: the backend's slot is published, and
discovery finds it, minutes before its model has arrived. Next and the
automatic move on therefore wait for the change. Once it is done the row
waits for a fresh read, which shows Installed. One install runs at a time,
since one request raises one prompt; the other Install buttons are
insensitive meanwhile. Dismissing the prompt puts the button back silently;
a refusal, a request snapd rejects (Myna without the flag: "feature flag
validation failed") or a change that fails puts it back with a toast whose
Details name the request, snapd's HTTP status (202 for a change that failed
after snapd accepted it) and snapd's error. Closing the wizard stops
following; snapd's change carries on.

A change snapd is already running to install a missing row's snap, started
in a terminal or by a wizard since closed, is followed the same way instead
of offering Install again. It is found in `/v2/changes?select=in-progress` as
an unfinished `install-snap` change whose summary names the snap. Such a change failing only puts Install back, since it
was not this window's action.

The subtitles size the download, in whole megabytes from 100 MB to 1 GB,
where a decimal is noise. The app's is the store's size of the `myna`
snap. The model's names the family and the size of what its install fetches:
the snap and the int8 model, or, with an NVIDIA GPU, the CUDA runtime and the
fp32 model, said as "up to" because the install hook falls back to the CPU
engine when the GPU has no driver. Once installed, that model's row names
only the family: which engine the hook picked is not known, and "up to"
reads wrong after the fact. The sizes are fixed per store revision in
`onboarding.rs`, not read from the store.

Installing may still happen elsewhere, so the component step re-assesses the
machine whenever the wizard regains focus, and every 2 s while something
required is missing. Next stays insensitive until the required components are
found; then the footer shows a success checkmark and "All required components
installed" left of it.

Both installs ask for `edge`, the only channel both snaps are published to,
and wait for the flag: snapd refuses to install a snap declaring a user
daemon unless `experimental.user-daemons` is set or its snap-id is on the
hardcoded allowlist in snapd's `overlord/snapstate/snapstate.go`, so an App
Center install of Myna fails on every stock machine. A plain install of the
model is a working backend: the install hook selects an engine, and selecting
one installs its model component.

The settings window never shows an install command either. When its
Diagnostics page finds Myna or every model missing (removed while the window
was open), it names what is missing over a "Set up Dictation" row whose suggested
"Set up" button opens this wizard (`win.setup`); the button marks the row as
the way on, and the whole row activates it. That group comes first on the page, above the
performance warnings and the report, since it is the only thing there to act
on; the copied report states the missing component without a command.

## Finishing setup

Leaving the component step makes a backend active and restarts the daemon, so
the shortcut step finds dictation running. When a re-assessment finds the last
missing component, the optional extension included, while the step shows,
the step does this by itself, then "All required components installed" shows
for a second and the wizard moves on. Only that transition counts: opening the
step with everything already installed waits for Next, so a re-run of the
wizard does not rush past it, and a machine whose extension the wizard
cannot install leaves the move to Next. Next during the pause moves on at once without
setting up again. Both snaps share a publisher, so
snapd's base declaration auto-connects `myna:backend` to the new backend's
slot and the step only restarts.

Setting up ends once the restarted daemon has claimed
`com.canonical.Myna.Dictation` again, a new owner of the name (0.4 s after
`systemctl --user restart` returns on Noble), watched on the shortcut step's
own proxy. Moving on as soon as the restart returned showed "Myna is not
running yet" on arrival until the daemon was up. A daemon that has not
claimed it after 10 s is left for the shortcut step to report.

Setting up usually takes a fraction of a second (a restart), so for its
first second the footer keeps "All required components installed" and only
Next goes insensitive; a spinner that flashed past read as a glitch. Past
that second a spinner and the stage below take the status's place, and the
header's back arrow goes until setting up ends: leaving then would strand a
connect or a restart in flight. The arrow stays through that first second,
since hiding it flashed it the same way; Back then lets the setup finish
without moving on.

A failed setup leaves the step as it was, Next retrying, and says so in a
toast whose Details open the report, as a failed install does: setting up
may have started by itself, and a dialog to dismiss would hold up a user who
only wants to go back or close. The report names the failed step (the
`systemctl --user` restart, or snapd's connect request and its HTTP status).
Dismissing the connect's polkit prompt is the user's answer, not a
failure: the step stays silently and Next asks again. Either way, until a
setup succeeds, a warning in the footer takes the check's place, "Dictation
is not set up yet. Select Next to try again.", since the toast times out and
the step would otherwise read as ready. The wizard's toasts rise above the
footer, never over its status or Next.

snapd shows that connection while the install change is still fetching the
model, and mounts the backend into Myna's namespace only as the change's
last task. A daemon restarted before then never finds the backend, although
it looks for it at every utterance. So setup first waits, spinner spinning,
until snapd's `/v2/changes?select=in-progress`, read as the user, lists no
unfinished change on Myna or a discovered backend, then decides on a fresh discovery. After 15 min it gives up with the
error dialog, naming the change, and Next retries. `snap_changes::parse_changes`
drops every ready change: that listing also returns a held one, `Hold` and
ready with no tasks, such as the auto-refresh of a snap removed while its
refresh was inhibited (stonking, 2026-09-30), and setup sat under it. Otherwise it runs the active-backend switch,
which costs one polkit prompt: snapd's `manage-interfaces` action is
`auth_admin_keep`, and the restart goes through `systemctl --user`, which needs
none.

The store auto-connects the backend's `hardware-observe` and
`system-observe` plugs (granted 2026-09-23), so the wizard connects neither.
With `hardware-observe` connected at install, the install hook's
`use-engine --auto` can pick the GPU engine, and on an NVIDIA machine the
install downloads the GPU components rather than the int8 model.

A spinner alone read as a hang, so beside it a line says what setup is
doing as it starts doing it (`active_backend::SetupStage`): checking, the
download in bytes or "Waiting for other software changes to finish…"
(snapd's summary of the change goes to the log only: it is English and
names snapd's internals), connecting
the model, with a reminder to authorize it since polkit's dialog can open
behind the wizard, and starting dictation. Closing the wizard stops a setup
still waiting on snapd, so nothing is connected or restarted behind it.
While the step polls, the same line shows snap's own error when it cannot
read the machine, rather than only reporting a component missing that it
could not check.

The assessment that opens the wizard or the settings window, each later one
that differs from the last, and each setup stage and outcome, is logged once as a GLib message in the `myna-config` domain: to
the journal when launched from the desktop (`journalctl --user -b | grep
myna-config`), and to stderr from a terminal.

## The keyboard shortcut

The daemon publishes `Activation` (`portal` or `control`) and, under the portal,
the portal's description of the binding as `Shortcut`. The last step and the Myna
page follow both through a live proxy, so a rebind elsewhere shows up at once.
Not running disables the button; no `Shortcut` from an older daemon is treated
as bound. A bound key reads "You can trigger Dictation anytime by using the
keyboard shortcut:" over its key caps on the last step; the other states say
what is missing instead.

**Portal.** Set up shortcut calls `BindShortcut("")`: the daemon offers `LOGO+j`
(Super+J) to the portal's dialog, because the portal files a binding under the
caller's app id and grants one only through that dialog. The description
(`Press <Super>j`) becomes key caps. Change shortcut opens
`gnome-control-center applications myna_myna`: GlobalShortcuts 1 has no
`ConfigureShortcuts` and no unbind. A refused bind shows the error dialog.
The portal lists the binding under the name the daemon gives it,
"Dictation (tap to start or stop)" (translated in `myna-desktop`). The portal
files the grant by the shortcut id `dictate` and keeps the name it was granted
with, so renaming it neither drops nor re-asks an existing grant (checked on
resolute, both ways between the old and new name).

The dialog belongs to the daemon, which owns the portal session, and is raised
with no parent window: the wizard is another process with no handle to lend
it. GNOME still centres it over the focused wizard and gives it focus
(resolute, 2026-09-30). Until a key lands the step holds no room for key caps,
so the sentence leads straight to Set up shortcut; the centred column grows by
one row of caps when a key lands, mostly while the portal's dialog covers it.

**Control (Noble).** The key is a GNOME custom shortcut to
`/snap/bin/myna.toggle`, the entry `myna.install-shortcut` writes; this
application is unconfined and writes it itself. Finishing setup installs
Super+J without asking once the restarted daemon reports `control`, unless
Myna's entry already has a key or another shortcut holds Super+J: a key the
user chose is never replaced silently. Under the portal only the portal's own
dialog grants a key, so arriving on the shortcut step with none bound raises
it, offering Super+J, over the step it concerns; the brief's "set the default
shortcut" cannot be silent there. A dialog dismissed there is the user's
answer and is not reported, unlike one raised by the step's button. While
no key is bound, Set up shortcut is the step's suggested action and Done is
plain, so finishing with nothing to trigger dictation is not the obvious
path. The key caps follow the key live on either path: the daemon's
`Shortcut` property under the portal, the custom shortcut's GSettings under
control, so a change in the settings window or in GNOME Settings shows at once. Set Up installs Super+J;
Change captures a key in a dialog that shows the default key as key caps for
its example and:

- takes a chord with Ctrl, Alt or Super, or a lone function or media key, so
  typing is never hijacked; media keys are stored as `XF86<Name>`, the only
  spelling the desktop resolves;
- inhibits the desktop's shortcuts while open, as GNOME Settings does, so keys
  GNOME uses reach it (GNOME asks once to allow this);
- asks before taking a key from a desktop action or another custom shortcut,
  and removes it there;
- refuses keys gsd-media-keys binds as `-static` (Super+O for rotation lock):
  it grabs those at login and holds them until logout whatever the setting
  says, so a replaced one would never fire.

## Cost

The startup assessment is the same two subprocesses as a startup
refresh, plus one read of snapd's socket and one D-Bus call to gnome-shell,
run before any window exists, and it is handed to the wizard rather than
repeated there. The shortcut proxy spawns nothing: it is one D-Bus match
per surface. Regaining focus on the component step costs a full re-assessment,
and so does each poll while a component is missing: a `snap list`, a
discovery, one read of snapd's socket for the flag, one `GetExtensionInfo`
call to gnome-shell (at most 2 s when it does not answer), a stat of each
extension directory and a scan of `/sys/bus/pci/devices` for an NVIDIA GPU.
While a snap row is missing and the flag is on, each re-assessment also
reads snapd's in-progress changes once, to follow an install started
elsewhere. An install reads its change once a second until snapd is done.
Setting up reads snapd's changes over its socket once, and again every 2 s
while snapd is still changing Myna or a backend, plus one discovery after
such a wait.
Reopening the wizard from the menu costs one assessment, and closing it one
startup-sized refresh of the settings window.
