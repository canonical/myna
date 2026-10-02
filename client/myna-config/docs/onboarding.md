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
the first carries a back arrow to the step before it. On first run Done
closes Myna Settings; opened from the settings window's menu, it closes only
the wizard, and the settings window stays and rediscovers. It closes each
window rather than quitting, so their close handlers still stop what they run.
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
of the socket interface they publish, not of their name. Installing turns
the flag on first, since snapd refuses Myna without it.

The extension is optional because dictation works without it: the daemon
falls back to desktop notifications. The myna-config deb, which this
application ships in, installs it to `/usr/share/gnome/gnome-shell/extensions`,
where it shadows Ubuntu's packaged copy on Stonking, so the wizard never
installs it and unavailable means a copy gnome-shell cannot run, not a
missing one. Only a system copy counts, not a development copy in `~/.local`.
Only a disabled copy is the wizard's to fix; every other state below is out
of its reach, and the component step skips the extension silently, since
dictation still shows its status in notifications. Its state is one of:

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
  wizard does not flip that switch, which would turn every
  one of the user's extensions back on. On the Ubuntu session this does not
  arise where the session mode (`/usr/share/gnome-shell/modes/ubuntu.json`)
  lists myna-shell, as Stonking's does: a mode extension runs and stays
  changeable whatever that switch says. It does on a series or session whose
  mode does not list it (measured on Noble's gnome-shell 46 with a system
  copy: `state` 6, `canChange` false, `UserExtensionsEnabled` false);
- failed: gnome-shell ran the system copy and it errored (`state` 3), or
  reports a state this code does not know;
- out of date: its `shell-version` lacks the running gnome-shell (`state` 4).
  It does not work with this version of GNOME;
- locked: the administrator locked it (`canChange` false with extensions on:
  `enabled-extensions` is not writable);
- unavailable: no system copy, or no gnome-shell answering on the session
  bus within 2 s. A shell that does not report `UserExtensionsEnabled` is
  taken as having extensions on.

gnome-shell sends `type` and `state` as doubles; `type` 1 is a system copy,
`state` 1 enabled, 2 and 6 disabled and never enabled. The transient 8
(activating) and 7 (deactivating) read as the state they are heading for, so
a re-read right after `EnableExtension` does not read it as out of reach.
Enabling itself waits past 8 for 1, since an activating extension may still
error.

The settings window's main menu reopens the wizard (Set Up Dictation), modal
over the window. It refuses while a backend operation is in flight: the wizard
connects a backend and restarts the daemon, and the window's operation gate does
not cover it. Closing the wizard rediscovers, since it may have changed both.

## Installing

The component step titles itself "Install components" over one paragraph,
the mock's ("Dictation requires some additional components, including the
optimal speech-to-text model for your computer. Installation might take a
few minutes."), and offers one button, "Install all components"; nothing is
left for a terminal. It is the step's suggested action while something
required is missing; with only the extension left it is plain and Next
leads. The components are not
listed one by one. Below the button a dimmed line sizes what installing
downloads (`onboarding::remaining_download`): the snaps still missing, in
whole megabytes from 100 MB to 1 GB, where a decimal is noise. The app's
share is the store's size of the `myna` snap; the model's is the snap and
the int8 model, or, with an NVIDIA GPU, the CUDA runtime and the fp32
model, said as "Up to" because the install hook falls back to the CPU engine
when the GPU has no driver. The flag and the extension download nothing, so
a machine missing only those shows no size. The sizes are fixed per store
revision in `onboarding.rs`, not read from the store.

The button runs every step the machine still needs, in order
(`onboarding::install_plan`): turning on snapd's flag, which snapd needs
before it installs Myna, then the app, the model and the extension. A step
already done is skipped, so a partly installed machine installs only what
is missing, and a machine with nothing left shows the button insensitive and
labelled "Installed". While the run goes the button is insensitive and reads
"Installing…", and a spinner below it names the step: "Enabling user daemons
support", "Installing Dictation app", "Installing speech-to-text model" or
"Enabling shell extension", with the download's percentage once one
announces its size. The button keeps one width through its three labels.
Next is insensitive meanwhile. After each step a fresh read of the machine
picks the next one; a step that succeeded is not retried when that read does
not show it yet (`onboarding::next_install`).

The steps cost as few polkit prompts as snapd allows. The flag goes through
snapd's REST API as the user (`PUT /v2/snaps/system/conf`), and snapd raises
polkit's prompt for `io.snapcraft.snapd.manage-configuration` itself: no
root code of ours. The prompt therefore shows snapd's wording ("access or
modify snap configuration"), not a Myna one. snapd answers only once the
prompt is, 40 s for one left open on Noble, so the write waits up to 10 min
for that answer (`SnapdTimeouts::authorization`) before following the
change; interface connects wait the same way. The flag is never turned off,
since snapd refuses Myna's refreshes without it.

The snaps are asked of snapd as the user (`POST /v2/snaps/<name>`,
`{"action":"install","channel":"latest/edge"}`), the same install `snap
install --edge` makes, and snapd raises polkit's prompt for
`io.snapcraft.snapd.manage` itself. That action is `auth_admin_keep` per
process, so the model installing right after the app asks nothing more: a
bare machine asks twice, once for the flag and once for the snaps. snapd
answers once the prompt is answered; the step then follows the change `GET
/v2/changes/<id>` once a second (`snap_install.rs`); ten failed reads in a
row end it, so snapd restarting mid-install is no failure. The percentage
is the bytes of every download task in the change over what they announce or
what the step expected to fetch, whichever is more, never going down. It
shows only while a download runs: mounting, hooks and services take 15 s or
more after the app's download, and 100% there read as stuck (seen on
Noble). The model's component download runs inside the backend's own
install change, fetched by its install hook's engine choice, so the model's
step follows it to the end, the percentage resuming where the snap's own
download left it. A download served from snapd's cache reports no bytes, so
a cached install shows no percentage.

The extension's step asks the user's own gnome-shell over the session bus
(`org.gnome.Shell.Extensions.EnableExtension`): no polkit, no prompt. The
call only adds the uuid to `enabled-extensions`; gnome-shell starts the
extension once that setting changes, so the step waits until
`GetExtensionInfo` reports it running, for up to 5 s. A system copy
gnome-shell already lists runs at once on X11 and Wayland alike, so no
re-login is asked for. An extension out of the wizard's reach (above) is not
a step and is never mentioned: it holds neither Next nor the move on.

A component snapd is still installing counts as missing
(`onboarding::while_installing`) whatever a read finds half-way: the
backend's slot is published, and discovery finds it, minutes before its
model has arrived. Next and the automatic move on therefore wait for the
change. Dismissing a prompt stops the run silently, the button offering
again what is still missing. A refusal, a request snapd rejects (Myna
without the flag: "feature flag validation failed"), a change that fails or
gnome-shell refusing the extension stops it with a toast whose Details open
the report, which names the failed step as what it was: snapd's request,
its HTTP status (202 for a change that failed after snapd accepted it) and
snapd's error, or the D-Bus call. The button then offers again, sized for
what is still missing. Closing the wizard stops following; snapd's change
carries on.

A change snapd is already running to install a missing snap, started in a
terminal or by a wizard since closed, is followed the same way instead of
offering the button: it reads "Installing…" over that step. It is found in
`/v2/changes?select=in-progress` as an unfinished `install-snap` change
whose summary names the snap. Such a change failing only puts the button
back, since it was not this window's action.

Installing may still happen elsewhere, so the component step re-assesses the
machine whenever the wizard regains focus, and every 2 s while something
required is missing and no run is going. Next stays insensitive until the
required components are found.

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
the shortcut step finds dictation running. When the button's run ends with
nothing left to install, or a re-assessment finds the last missing component
while the step shows, the step does this by itself, then the button's
"Installed" shows for a second and the wizard moves on. An extension out of
reach does not hold that move. Only those transitions count: opening the
step with everything already installed waits for Next, so a re-run of the
wizard does not rush past it, and so does a disabled extension found elsewhere
to be the last piece. Next during the pause moves on at once without
setting up again. Both snaps share a publisher, so
snapd's base declaration auto-connects `myna:backend` to the new backend's
slot and the step only restarts.

That restart is skipped while a shortcut dialog is up (the daemon's
`ShortcutDialog`, or a bind of this process still waiting): a
reopened wizard moving past the step while the Myna page row's dialog was up
killed the daemon under it, so the row's bind failed with D-Bus `NoReply` and
the dialog stayed on screen with nobody waiting for it (resolute, 2026-10-01).
A daemon with a dialog up is already running, and the wizard moves on without
waiting for a new owner. A switch that connects a backend still restarts.

Setting up ends once the restarted daemon has claimed
`com.canonical.Myna.Dictation` again, a new owner of the name (0.4 s after
`systemctl --user restart` returns on Noble), watched on the shortcut step's
own proxy. Moving on as soon as the restart returned showed "Myna is not
running yet" on arrival until the daemon was up. A daemon that has not
claimed it after 10 s is left for the shortcut step to report.

Setting up usually takes a fraction of a second (a restart), so for its
first second only Next goes insensitive; a spinner that flashed past read as
a glitch. Past that second a spinner and the stage below show under the
button, and the
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
setup succeeds, a warning under the button says "Dictation is not set up
yet. Select Next to try again.", since the toast times out and the step
would otherwise read as ready. The wizard's toasts rise above the footer,
never over Next.

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
download in bytes or "Waiting for other software changes to finish"
(snapd's summary of the change goes to the log only: it is English and
names snapd's internals), "Setting up speech-to-text model" while it
connects the backend, and "Starting dictation". These use the install
steps' words, never the engine's name, which a first-run user has not been
told, and like the install steps they carry no ellipsis; only the button's
"Installing…" has one, as in the mock. Closing the wizard stops a setup
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

**Portal.** Set up shortcut asks the daemon to bind with no preferred key: it
offers `LOGO+j` (Super+J) to the portal's dialog, because the portal files a
binding under the caller's app id and grants one only through that dialog. The
description (`Press <Super>j`) becomes key caps. Change shortcut (Change on
the Myna page row) raises the same dialog, modal to its window, offering the
current key: GlobalShortcuts 1 has no `ConfigureShortcuts`, and from GNOME 48
the portal hands binds to GNOME Settings, which raises its dialog only for a
shortcut id it stores no key for (`cc_global_shortcut_dialog_present`). So
Myna Settings takes Myna's `dictate` entry out of that store
(`/org/gnome/settings-daemon/global-shortcuts/<app id>/shortcuts`,
`adapters/portal_shortcuts.rs`) and flushes it before asking the daemon to
bind. The portal has filed the snap's daemon under `myna_myna` and under `.`
(an empty app id, GNOME 49), so the entry is taken out under both, and under
any listed app id naming Myna, whichever the daemon's session has; the dialog
offers the key stored under `myna_myna` first. With no entry stored at all
GNOME raises the dialog anyway, so Change is the same plain bind. The
dialog's session binds the new key and closes, while the daemon listens on
its own long-lived session, so on success Myna Settings calls the portal
backend's `org.gnome.GlobalShortcutsRebind.RebindShortcuts` for each app id
the dialog stored a key under, as GNOME Settings does after an edit: the
daemon gets `ShortcutsChanged` and the new key works at once, the old one no
longer. Every entry the dialog did not replace is put back, and on any other
answer (cancel, a refusal, a failure, a closed window) all of them are; the
app holds itself open until then. The dialog is GNOME Settings' own, titled
"Add Keyboard Shortcuts" with an Add button even when a key is being
changed; Myna cannot word it. It lives here, not in the daemon, because the
confined daemon can write neither gnome-settings-daemon's dconf nor the
portal backend's interface, and because it then works with an older snap
daemon too. Only where gnome-settings-daemon's schemas are missing, or GNOME
Settings' `org.gnome.Settings.GlobalShortcutsProvider` neither runs nor can
be started (the portal then answers binds itself, so the store means
nothing), does Change open `gnome-control-center applications myna_myna`.
Measured on 26.04 (GNOME 50, store and tree daemons alike, 2026-10-01). Two edges remain: Myna Settings
killed under the dialog leaves the entry out until the user answers it, and an
older daemon that gives up after 120 s gets the entry put back with the sheet
still up, so an Add there stores the new key without moving the live grab
until the next login. A bind the user asked for that fails
shows a "Could not set up the shortcut" toast ("Could not change the
shortcut" for Change) whose Details open the daemon's
own words under a plain summary, as the other failure toasts do. Cancelling the
dialog is not a failure and shows nothing: GNOME answers Cancel with the
portal's "other" response, which ashpd words "Portal request didn't succeed
with no information", so that reply (and a "cancelled" one) reads as declined;
a backend that answers "other" for a real fault is silent too, and the daemon's
log keeps its words.
The portal lists the binding under the name the daemon gives it,
"Dictation (press to start and stop)" (translated in `myna-desktop`), the
same words as the Myna page's "Press to start and stop". The portal
files the grant by the shortcut id `dictate` and keeps the name it was granted
with, so renaming it neither drops nor re-asks an existing grant (checked on
resolute, both ways between the old and new name).

The dialog belongs to the daemon, which owns the portal session, so the wizard
lends it its window: it exports its toplevel (an xdg-foreign handle on Wayland,
the XID on X11) and calls `BindShortcutWithParent`, which makes the dialog
modal to the wizard instead of a window that gets lost behind it. A daemon
without that method answers `UnknownMethod` and the wizard falls back to
`BindShortcut`, as before. An export the compositor never answers falls back
to no parent after 2 s.

GNOME's dialog only ever closes on the user's answer, so the daemon raises one
at a time and publishes `ShortcutDialog` while it is up. Every surface follows
that property: Set up shortcut (or Change) and Done are insensitive while it is
true, whichever window raised the dialog. A step that did not raise it says
why ("The desktop's dialog to confirm a keyboard shortcut is already open.
Answer it to continue."); the Myna page row reads "Waiting for the desktop's
shortcut dialog" whichever surface raised it, since an insensitive Set up
beside "Not set up" reads as broken. Arriving on the step under such a dialog raises
nothing, and a bind that loses the race to it is refused by the daemon and
reads as the same wait, not an error. Before this, waiting it out or reopening
the wizard stacked a second and third dialog (resolute, 2026-10-01).
Closing the wizard under its own dialog takes the dialog down with it, since a
parented dialog does not outlive its parent, and the bind ends as dismissed;
an unparented one (an older daemon's) stays up.

The surfaces of one Myna Settings process (the wizard and the Myna page row)
also share their own binds in flight, so either holds while the other's
dialog is up even when the daemon publishes nothing. A surface closed while its
bind waits still releases that hold when the bind ends.

**A dialog left open.** A daemon without `ShortcutDialog` gives up on its
dialog after 120 s and answers "shortcut bind unanswered", and a daemon that
exits under its dialog never answers (`NoReply`); GNOME keeps the dialog up
either way, and nothing says when it is answered. No error shows: every
surface goes back to Set up, sensitive, the step saying "If the desktop's
dialog to confirm a keyboard shortcut is no longer open, set up the shortcut
again." and arriving on it raising nothing by itself. A key landing or a new
bind clears that. Holding the button instead left a user who cancelled the
dialog at a dead end until they went Back. A closed and reopened Myna
Settings can still stack dialogs against an older daemon: only the daemon
sees both processes.

Until a key lands the step holds no room for key caps, so the sentence leads
straight to Set up shortcut; the centred column grows by one row of caps when a
key lands, mostly while the portal's dialog covers it.

**Control (Noble).** The key is a GNOME custom shortcut to
`/snap/bin/myna.toggle`, the entry `myna.install-shortcut` writes; this
application is unconfined and writes it itself. Finishing setup installs
Super+J without asking once the restarted daemon reports `control`, unless
Myna's entry already has a key or another shortcut holds Super+J: a key the
user chose is never replaced silently. Under the portal only the portal's own
dialog grants a key, so arriving on the shortcut step with none bound raises
it, offering Super+J, over the step it concerns; the brief's "set the default
shortcut" cannot be silent there. Dismissing that dialog is the user's
answer and is not reported, nor is a failure of that bind, unlike one the
step's button raised. While
no key is bound, Set up shortcut is the step's suggested action and Done is
plain, so finishing with nothing to trigger dictation is not the obvious
path. The key caps follow the key live on either path: the daemon's
`Shortcut` property under the portal, the custom shortcut's GSettings under
control, so a change in the settings window or in GNOME Settings shows at once.

On the step, Set up shortcut and Change shortcut capture the key in place: the
key caps become a "Press the new shortcut…" box ("Press a shortcut…" when
there is none), the button becomes Cancel, and Done is insensitive until the
capture ends. Escape, Cancel or leaving the step keeps the key there was; a
refused key turns the box's outline red and is explained under it, in room kept for two
lines so the page does not move, and the capture goes on. Declining a swap
clears that explanation and keeps waiting. A key chosen here survives Back then Next:
the setup that runs on moving on installs Super+J only where no key is set. The settings window's row keeps
a dialog: a row has no room for the prompt and the refusal, and GNOME
Settings captures its own shortcuts in a dialog too. There Set Up installs
Super+J and Change opens the dialog, which shows the default key as key caps
for its example. Either way the capture:

- takes a chord with Ctrl, Alt or Super, or a lone function or media key, so
  typing is never hijacked; media keys are stored as `XF86<Name>`, the only
  spelling the desktop resolves;
- inhibits the desktop's shortcuts while it waits, as GNOME Settings does, so
  keys GNOME uses reach it (GNOME asks once to allow this);
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
While a snap is missing and the flag is on, each re-assessment also
reads snapd's in-progress changes once, to follow an install started
elsewhere. An install reads its change once a second until snapd is done.
Setting up reads snapd's changes over its socket once, and again every 2 s
while snapd is still changing Myna or a backend, plus one discovery after
such a wait.
Reopening the wizard from the menu costs one assessment, and closing it one
startup-sized refresh of the settings window.
