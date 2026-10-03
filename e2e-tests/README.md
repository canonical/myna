# Myna Settings end-to-end tests

The real Myna Settings binary driven through its user flows (onboarding,
backend switching, model settings) on fresh GNOME desktop LXD VMs, one per
series, with the store's latest/edge snaps. Assertions are machine state
(snapd, systemd, dconf); screenshots are artifacts for a human.

```sh
make e2e                                  # noble, every suite
make e2e E2E_RELEASE=resolute SUITES=backend-switch
e2e-tests/vm/provision.sh --release noble --force   # after an edge bump
```

Needs LXD with KVM and the `myna-noble` workshop (`workshop launch
myna-noble`), which builds the binary. The first run provisions the VM
(about 15 minutes) and snapshots three states, `bare`, `components-only`
and `installed`; every suite then restores the one its `# snapshot:` header
names. Artifacts (screenshots, app and polkit logs, the journal) land in
`e2e-tests/.run/artifacts/<release>-<suite>/`.
