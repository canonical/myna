# Myna Settings end-to-end tests

Real-machine flows of Myna Settings on fresh per-series GNOME desktops:
onboarding, backend switching, model settings, and (later) the shell
extension, the shortcut portal and a full spoken dictation. The design
marries two proven ancestors:

- authd's e2e architecture (`~/probe/ubuntu/authd/e2e-tests`): per-release
  desktop VMs provisioned from Ubuntu cloud images, libvirt snapshots as
  test states, one entry point shared by local runs and CI.
- the Settings/onboarding harness (`~/myna-onboarding-stable/tools`,
  `~/myna-config-ux-round3/tools`): an AT-SPI step driver under Xvfb,
  deterministic polkit answers via temporary rules and a cancelling agent,
  and host-side assertions over SSH. Those directories are the prototype;
  this directory is the ported, maintained copy. Port notes live in each
  file's header.

No YARF/OCR: Myna Settings never touches the greeter, and AT-SPI selectors
are deterministic where pixels and OCR are not.

## Layout

- `vm/` - cloud-init templates, libvirt domain, `provision.sh`.
- `tools/` - the driver: `shot.sh`/`shot-remote.sh` (build, ship, run under
  Xvfb on the VM's session bus), `shot-driver.py` (step language),
  `cancel-agent.py`, `snapd-rest.py`, `realshot.py`, `rdkeys.py`,
  `fake-daemon.py`, `slow-snapd.py`, `check-dictation.sh`.
- `suites/` - one executable per flow, with a `# snapshot: NAME` header
  declaring the machine state it starts from. `lib.sh` gives `shot`, `on`,
  `assert`, `assert_ssh`, `suite_status`.
- `run-suite.sh` - revert -> boot -> run -> collect artifacts.
- `.run/` - gitignored: SSH key, build cache, VM artifacts, screenshots.

## Usage

```sh
# One-time per release (~1 GB download, 16 GB sparse image):
e2e-tests/vm/provision.sh --release noble

# Run a suite (builds myna-config in the myna-noble workshop first):
e2e-tests/run-suite.sh --release noble --suite onboarding-full

# Iterate without rebuilding:
e2e-tests/run-suite.sh --release noble --suite backend-switch --no-build
```

Provisioning ends with three disk snapshots mirroring the harness's
`state.sh` states: `bare` (no myna anything), `components-only` (flag +
snaps, unconnected), `installed` (connected, daemon restarted, shortcut on
noble). Suites start from one; `run-suite.sh` reverts, so nothing leaks
between runs. Reprovision with `--force` after a store edge bump or a stale
image.

## Writing a suite

Suites are bash. Assert machine state over SSH (`assert_ssh`); use
`waitfor:`/`gone:` and screenshots at UI checkpoints only, so toolkit pixel
drift cannot flake a suite. Remember the accessible role for plain buttons
differs by series (`$BTN` in `lib.sh`), and polkit outcomes belong to the
rules (`--polkit`, `--apply-polkit`), not to typed passwords.

## CI status

Phase 1 (this directory): local runs against local libvirt VMs.
Phase 2 wires this into GitHub Actions authd-style: cached qcow2 per series
pushed to ghcr via oras, a per-series deb build, nightly matrix over
noble/resolute/stonking, PR runs gated by an `e2e-tests` label.
