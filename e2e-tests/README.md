# Myna Settings end-to-end tests

The real Myna Settings binary driven through its user flows (onboarding,
backend switching, model settings) on fresh GNOME or Xubuntu desktop LXD VMs,
one per series and desktop, with the store's latest/edge snaps. Assertions are machine state
(snapd, systemd, dconf); screenshots are artifacts for a human.

```sh
make snap-fake                            # once: the dictation suite's backend
make e2e                                  # noble, every suite
make e2e E2E_RELEASE=resolute SUITES=backend-switch
make e2e E2E_DESKTOP=xubuntu              # Xfce on X11 (noble)
make e2e E2E_VM_MEMORY=4GiB E2E_MYNA_SNAP=$PWD/myna-snap/myna_*.snap   # smaller VM, the tree's snap
e2e-tests/vm/provision.sh --release noble --force   # after an edge bump
```

The `dictation` suite is the one that dictates: a speech clip played to a
virtual PipeWire microphone, the daemon connected to the fake backend snap
(`make snap-fake` builds it; `E2E_FAKE_SNAP=PATH` names another), and the
text asserted in a GTK field on the real desktop session.

Needs LXD with KVM and the `myna-noble` workshop (`workshop launch
myna-noble`), which builds the binary. The first run provisions the VM
(about 15 minutes) and snapshots three states, `bare`, `components-only`
and `installed`; every suite then restores the one its `# snapshot:` header
names. Artifacts (app and polkit logs, the journal, burst frames) land in
`e2e-tests/.run/artifacts/<release>-<suite>/` (`<desktop>-<release>-<suite>`
for Xubuntu, whose VM is `myna-e2e-xubuntu-<release>`).

Screenshots at fixed checkpoints (`<suite>/<NN>-<checkpoint>.png`) are
collected in `e2e-tests/.run/artifacts/screenshots/`. On Xubuntu they are of
the real desktop, pill included; on GNOME of the app under Xvfb. CI compares
the Xubuntu set with the last approved nightly in a contact sheet
(`tools/contact-sheet.py`; see AGENTS.md for how to re-baseline). Build one
by hand with `python3 -I e2e-tests/tools/contact-sheet.py --current A
--baseline B --out sheet.html`.

## Trying a branch by hand

`make try-desktop` is for a person, not a suite: a persistent desktop VM
(`myna-try-<desktop>-<release>`, a copy of the e2e VM's `installed` snapshot,
so the e2e VMs are never touched) running the myna snap and myna-config deb
built from `HEAD`, with the host's microphone and speakers.

```sh
make try-desktop DESKTOP=xubuntu RELEASE=noble            # create (first: snap + deb builds), start
make try-desktop DESKTOP=xubuntu RELEASE=noble ACTION=console   # open the desktop (remote-viewer)
make try-desktop ACTION=update      # after committing: rebuild, install, restart daemon and HUD host
make try-desktop ACTION=reset       # back to the machine `up` left (`fresh`)
make try-desktop ACTION=down        # stop it (kept); ACTION=delete removes it
```

`DESKTOP=gnome` and `RELEASE=resolute` work the same. It refuses to start or
build below 6 GiB of available RAM or 15 GiB free on `/`, and runs with 4 GiB
(`TRY_MEMORY`). Artifacts are built from the commit, not the working tree.
Audio goes over pulse TCP on the LXD bridge while a try VM runs
(`ACTION=audio` re-establishes it); `dev/try-desktop.sh --help` has the
reasoning and the exposure trade-off.
