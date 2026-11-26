# Preface

Read this before changing the Myna Settings and dictation end-to-end tests.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

A suite runs on the host: `run-suite.sh` restores the LXD snapshot it
declares, pushes the binaries and `tools/` into the VM, and the suite calls
`shot` (the app driven by `shot-driver.py` steps) and asserts with `assert_on`
inside the VM. CI runs the same `make e2e` per series and desktop
(`.github/workflows/e2e.yml`); the suites are desktop-aware, and the places
the desktops legitimately differ (`assert_shortcut_bound`,
`assert_input_method_up`, the `shot` display) are helpers in `suites/lib.sh`.

The desktop is an axis, `E2E_DESKTOP=gnome|xubuntu|kde` (`--desktop` on
the scripts). Its cloud-init is `vm/user-data.common` merged with
`vm/user-data.<desktop>` by `lib.sh`; session readiness (`session_up`) and
the display environment `on_vm` hands out are per desktop. Xubuntu is Xfce on
X11 (lightdm autologin). KDE is Kubuntu's Plasma 6 on Wayland (sddm autologin
into the `plasma` session, `kubuntu-desktop` without its application
Recommends); `on_vm` gives it `WAYLAND_DISPLAY=wayland-0`. Its `installed`
stage also makes IBus KWin's input method, as onboarding does (a bare Plasma
has none). `spectacle -b -n -f -o FILE` through `on_vm` photographs the real
session (KWin's ScreenShot2 is restricted to Spectacle's desktop entry).
Wayland gives no client a view of other windows, so the dictation helpers
ask KWin: a KWin script (`org.kde.kwin.Scripting.loadScript`) prints the
window list into the user journal (`kwin_windows`). KWin activates a window
launched through `on_vm` on map, and the Qt field app's own `activateWindow()`
moves between its windows, so `field_focus` only waits for the app to say so.
The Qt app reports `active` on each window's `ActivationChange`: its
`applicationStateChanged` can precede `activeWindow()`, which right after
login left the report stale with KWin's window active. Kate
needs a document named on its command line: session restore reopens it on
its Welcome page, which has no text field.

## Screenshots

`shot` runs the app on the real display of an X11 desktop (Xubuntu: `shot.sh
--display real`, so a screenshot shows the panel, theme, pill and
notifications) and under its own Xvfb on GNOME and Plasma, whose Wayland
sessions xdotool cannot drive. There only the windows go to Xvfb: the app
keeps the session's `WAYLAND_DISPLAY`, which picks Plasma's profile and which
`ibus start --type kde-wayland` requires. A checkpoint is a fixed name,
`shot:shots/<suite>/<NN>-<checkpoint>.png`; the dictation suite takes its
pill checkpoints (listening, finishing, notice, error, with the backend held
still by SIGSTOP so `finalizing` lasts) with `checkpoint NAME` on Xubuntu
and Plasma. `run-suite.sh` moves them to `.run/artifacts/screenshots/<suite>/`; the
workflow uploads them as `screenshots-<desktop>-<release>` (30 days), beside
the whole `.run/artifacts/` upload.

The nightly `contact-sheet` job runs `tools/contact-sheet.py` on the Xubuntu
noble and the Kubuntu resolute sets, each against the last approved one: one self-contained HTML page, each
checkpoint current beside baseline, with NEW, MISSING, changed and same
badges (same = identical bytes; the panel clock makes most differ, so it is a
page to look at). "Approved" is the newest green scheduled or dispatched E2E
run on `main` that kept `screenshots-xubuntu-noble`. To re-baseline after an
intended UI change, merge it and dispatch E2E on `main` (`make ci-e2e` from
main, or the Actions tab); once that run is green it is the baseline. Tests
never assert pixels.

The `dictation` suite does not use `shot`. It runs `tools/field-app.py` (plain
and password entries that report themselves as JSON; GTK4, or Qt6 on Plasma,
whose apps reach KWin through text-input-v2 rather than v3) in the real
session, a speech clip looped on a virtual PipeWire speaker whose
loopback is the default microphone (`dictation` provisioning stage), and the
fake backend snap, so the transcript is scripted. Lost audio is not
asserted here: PipeWire hands a capture client each graph cycle (21 ms) once,
and a virtual runner's host stops the vCPUs for longer now and then, so the
loss is real but says nothing about the code; capture continuity belongs to
lab hardware (Testflinger). The daemon still ends a dictation that lost audio,
so a failed suite whose daemon logged `ctrl: capture lost` runs once more,
keeping the first attempt as `<suite>-lost-audio`. Every suite's artifacts
carry `out/cpu-guest.log` and `cpu-host.log` (`tools/cpu-sample.sh`): busy,
steal and CPU pressure a second, in the VM and on the host. PipeWire still
runs realtime to make such losses rarer: linger starts PipeWire before the login session, when RTKit grants nothing,
so the same stage puts `ubuntu` in the `pipewire` group, whose rlimits
PipeWire ships. The daemon is the store's
edge myna snap unless `E2E_MYNA_SNAP=PATH` names one built from the tree
(`make snap-myna`), which `run-suite.sh` installs over it in each run. It drives the daemon through
its D-Bus `Toggle` and reads `State`/`AudioPeak` from it; one case presses a
bound key through `tools/uinput-keys.py`, a virtual keyboard on the real seat
(the same on both desktops; it also presses Escape to leave GNOME's initial
Overview, where a new window is not focused). Helpers are in
`tools/dictation-lib.sh`. On Xubuntu and Plasma it also runs the
`myna-hud-host` supervisor from the build (what the deb's autostart entry
runs) so the HUD pill exists; the VM has no deb. CI builds the tree's myna snap and passes it
as `E2E_MYNA_SNAP`, so the daemon and HUD under test are the tree's, not
edge's.

The pill case (Xubuntu, Plasma) waits for the HUD to register with the daemon
(`hud_registered`) before dictating, since until then the daemon toasts
instead; it then asserts the host kind (`--host x11` or `layer-shell`), that
the pill is mapped, takes no input focus (an X11 notification-type window, a
layer surface on KWin's overlay layer) and sits bottom centre in the work
area, that the field app stays active, that no Myna toast was sent (a
`dbus-monitor` watch, with a positive control) and that the HUD's log has no
GTK critical. The suite ends by logging out: the host and HUD must go.

On Plasma the Qt field app keeps `other` in a window of its own, so the
focus-loss cases move focus between windows. Inside one window, Qt's
text-input-v2 tells KWin a new field was entered, KWin passes on only a
state update (and a `reset`, which IBus's Wayland panel drops), so IBus never
sees a focus change and the rest of an utterance lands in the new field. A
KWin bug; move `other` back once it is fixed.

The `upgrade` suite is the one that keeps the store's myna (`# myna: store`:
`run-suite.sh` installs no `E2E_MYNA_SNAP` for it) and runs only with
`E2E_UPGRADE_CHANNEL` set (`# requires:`). From the `dictation` snapshot it
refreshes every store snap to that channel in one change with the HUD up,
then checks revisions, components, connections and services, and dictates.
CI runs it alone in a dispatched E2E run that `snaps.yml` starts after
releasing a candidate to a branch. A sideloaded snap cannot test this: it
keeps no store components and skips the old revision's refresh-mode.

The `plugins` suite follows a text plugin's life from the store, on the
`chains` snapshot over a fake speech backend: both text snaps installed from
edge and connected, then through Settings Get more steps → Translate →
Français (a chain made and used, the model installed with one polkit grant),
Tidy installed and added in stage order, a dictation typing French while a
provider outside the allow-list is ignored, Remove on the language page (the
step stays, says what it needs, and the dictation types what was heard), and
Install from the chain's editor healing it. About half of component changes
empty the running daemon's backend mounts (snapd LP#2175592) and the daemon
then restarts itself once idle (`myna_desktop::restart`); the suite never
restarts it and asserts only the outcome. Each shot that changes a component
stays open until the daemon's `InstalledSteps` shows the change: Settings
asks the daemon to rescan after snapd is done, and closing it first skips
that.

The `deadair` suite takes `E2E_PLUGIN_BLOBS` (the sideloadable text snaps and
their components, what `snapcraft pack` leaves in `text-snap/` and
`translation-snap/`) and installs both
text snaps with their models over the fake speech backend, so what it
measures is Myna's own wait: `dead_air_ms` minus `asr_tail_ms` from the record
of past dictations (the first dictation after five daemon starts, then
`E2E_DEADAIR_N` warm ones, for Tidy and Tidy then French; bounds 1000 ms cold,
p95 100 ms and 1000 ms warm). It then breaks the text model (stopped,
SIGSTOPped, and `tools/fault-text.py` passing for `myna-text` while answering
with a chat reply, control characters, one word, nothing, or never) and asserts
every dictation still types the words that were heard and that the daemon's
journal holds none of them. The fake's scripted transcript is nine words; a
long dictation's wait is the last sentence's, which the bound already covers.

The `chains` suite drives the Processing tab like a user, on the `chains`
snapshot (`dictation` plus the French, Farsi and Simplified Chinese language
packs and the fake snap installed again). It makes a chain from New chain…,
adds Translate from the searchable language page and Tidy from the catalog,
installs their models from the store through Settings (one polkit grant each;
`run-suite.sh` pushes the catalog snapshots the deb installs, since the VM has
no deb), switches the chain on and off from its row, deletes and undoes,
removes a model from Diagnostics' Models groups, and dictates French, Chinese
and Farsi through the fake backend's scripted transcript (`snap set
myna-fake-backend transcript=TEXT`, one segment; empty keeps the default
script). Every case asserts the keyfile and the daemon (`ChainStatus`,
`--status`, the typed text), and each state is a checkpoint. Settings is also
run in fr_FR, zh_CN and fa_IR with the catalogs compiled on the host and
pushed (`MYNA_CONFIG_LOCALEDIR`); the selectors are the translations
(`tr_po`), falling back to the English msgid. A store install makes the
running daemon lose its backend mounts (snapd LP#2175592) or miss a share
connected after it started; the daemon restarts itself once idle
(`myna_desktop::restart`), and the suite asserts that rather than restarting
it. A shot run that starts an install stays open until snapd has the
component (`slowpoll`): closing Settings first drops the request.

`dev/try-desktop.sh` (`make try-desktop`) reuses the provisioning above for a
manual-test VM; it only ever copies the `installed` snapshot of an e2e VM.

## The fleet

One provisioned VM per (release, desktop), named by `vm/lib.sh`'s `vm_name`:
`myna-e2e-<release>` for GNOME, `myna-e2e-<desktop>-<release>` otherwise. Each
holds two snapshots: `bare`, the machine onboarding installs onto, and `ready`
(the `--snapshot` name), everything installed. `provision.sh` keeps its six
stage snapshots only while it runs, as a resume cache, and deletes them at the
end unless `--keep-stages`: on a `dir` pool each is a full 20 GiB disk.

Suites never run on a base. `run-suite.sh` copies it, runs, and deletes the
copy, so a run costs one disk for its duration. Each suite has its own copy
(`<vm>-run-<suite>`), so `E2E_JOBS=N` runs N suites side by side, each
logging to `.run/artifacts/<tag>-<suite>.run.log` with its lines prefixed
on the terminal (on chug, `make e2e E2E_RELEASE=resolute E2E_JOBS=3` ran
the fifteen in 13 minutes, a snap rebuild included). Every copy takes
`E2E_VM_MEMORY`, so mind the host; CI keeps one at a time. The bases are the warm cache
that makes a run reproducible without a 25-minute provision.

A stage snapshot on `dir` is a copy of the whole disk, which is how three
provisioned VMs once held 460 GB. The fleet therefore lives on a
copy-on-write pool when there is one:

    lxc storage create e2e zfs source=/var/snap/lxd/common/lxd/disks/e2e.img size=180GiB

`vm/lib.sh` picks it up (`E2E_POOL=e2e`) and `provision.sh` passes it through.
`fleet.sh list` shows which bases exist, their snapshots and their size;
`fleet.sh delete --release R --desktop D` takes one down.

A suite that does not test the store's snap needs `E2E_MYNA_SNAP`, the snap
`make snap-myna` built at this tree's HEAD: `run-suite.sh` now refuses to run
one without it, because the store's edge revision does not carry the settings
the tree writes and the suite would silently test the wrong daemon.

# Important

- The GNOME VM keeps the name `myna-e2e-<release>`; other desktops are
  `myna-e2e-<desktop>-<release>`, so they coexist. A new desktop adds a
  `user-data.<desktop>`, a `session_up` arm and, unless its suites draw on
  their own Xvfb, an `on_vm` arm in `lib.sh`; shared packages go in `user-data.common`.
- Every pull-request job blocks: GNOME on noble, resolute and stonking,
  Xubuntu on noble and Kubuntu on resolute, gathered into `e2e-result`, the
  one check to require (a repo setting). It passes when they all pass, or
  when the pull request touches none of the paths the `changes` job lists.
  Kubuntu on stonking runs nightly only; other Xubuntu releases are not in
  the matrix yet. Keep GNOME's job names stable.
- `E2E_VM_MEMORY` (default 6GiB, CI's) sizes every VM copy; a laptop that is
  also somebody's desktop uses `E2E_VM_MEMORY=4GiB`, one running VM at a time.
- `E2E_VM_CPUS` (default 3) gives every VM copy one vCPU fewer than CI's
  4-core runner: with all four, the runner took them from the guest and
  capture lost audio.

- Assert machine state (`assert_on`), never pixels. `waitfor:`/`gone:` gate
  the flow; a suite that needs a long `wait:` is missing a waitable
  condition.
- Polkit outcomes come from `shot --polkit allow|deny|cancel` (a temporary
  rule covering snapd's actions and the apply-plan and set-up pkexec, plus
  `cancel-agent.py` for cancel), never from typing into dialogs. The rule
  file must not survive a run; `shot.sh` removes it on exit.
- Accessible names are translated strings: a copy change breaks a selector
  by design, and the fix is the selector. Plain buttons are `push button` on
  noble's GTK 4.14 and `button` later (`$BTN`).
- The binary is built once in `myna-noble` (the GTK 4.14/adw 1.5 floor) and
  runs on every series, so Noble's bugs cannot hide behind a newer build host.
- A dictation case ends when the daemon says so (`State`), never after a
  sleep; the only sleeps are negative checks ("nothing arrived"), which wait
  out the time text would have taken.
- A suite also fails when its VM's journal holds an AppArmor denial of one of
  our snaps (`tests/lib/apparmor_denials.py`, run by `run-suite.sh` on the
  journal it saves). The inference snaps come from edge, so their denials
  count too.
- Snapshots are taken stopped. A suite that needs a new machine state gets a
  new provisioning stage, not setup in the suite.
- Each provisioning stage ends by saving the VM's snapd download cache to
  `.run/snapd-cache/` (blobs named by sha3-384, snapd's key), and every suite
  from `bare` is seeded with it, so onboarding installs from the store
  without fetching what provisioning already fetched. A new edge revision
  misses and downloads as before. onboarding-full keeps each install change's
  tasks in `out/install-tasks.txt`.
- `tools/` descends from the prototypes in `~/myna-onboarding-stable/tools`;
  driver oddities there (`harness.md`) are measured toolkit behaviour.
