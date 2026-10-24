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
window list into the user journal (`kwin_windows`). A window launched through
`on_vm` is usually activated on map, but not always: wait for the app to say
it is active, and fall back to a script's `workspace.activeWindow = w`. Kate
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
fake backend snap, so the transcript is scripted. PipeWire must run realtime
(a precondition) and no dictation may lose audio: on a loaded runner a
SCHED_OTHER graph drops cycles, which the daemon reports as lost capture.
Linger starts PipeWire before the login session, when RTKit grants nothing,
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

`dev/try-desktop.sh` (`make try-desktop`) reuses the provisioning above for a
manual-test VM; it only ever copies the `installed` snapshot of an e2e VM.

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
- `tools/` descends from the prototypes in `~/myna-onboarding-stable/tools`;
  driver oddities there (`harness.md`) are measured toolkit behaviour.
