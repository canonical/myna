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
names. Artifacts (screenshots, app and polkit logs, the journal) land in
`e2e-tests/.run/artifacts/<release>-<suite>/` (`<desktop>-<release>-<suite>`
for Xubuntu, whose VM is `myna-e2e-xubuntu-<release>`).
