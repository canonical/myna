# Spread Decision Record (feature 006, US5 / FR-014)

**Date**: 2026-08-10
**Verdict**: **ADOPT**

Time-boxed spike against the five spec criteria. References: snapcore/spread
source (`spread.yaml` self-test suite), snapd's upstream usage, local
snapcraft/LXD/KVM availability.

## Criteria assessment

1. **Clean-system lifecycle** — Spread's core model: per-task
   `prepare`/`execute`/`restore` running on a freshly allocated system;
   `restore` discards or resets state between tasks. Meets the requirement
   that the confined e2e starts from a clean install every run.

2. **Multi-system matrix** — Backends declare `systems:` (e.g.
   `ubuntu-24.04-64`); adding a release is one line. qemu backend covers the
   supported-Ubuntu matrix locally and in CI.

3. **Hosted-runner CI feasibility** — The qemu backend needs `/dev/kvm`;
   `ubuntu-latest` hosted runners expose KVM (snapd's own CI relies on this).
   Verified locally that `/dev/kvm` is present; the nightly workflow asserts
   it before running. Feasible.

4. **Virtual audio (constitution II)** — Spread VMs have no microphone; the
   task stands up a PipeWire virtual source in the guest and drives the
   fake-adapter backend, so no physical hardware is required and the same
   task runs unchanged on a workstation VM.

5. **Debug ergonomics** — `spread -debug` drops into an interactive shell on
   the VM at the failure point; `-reuse` re-runs against the same allocated
   system. Adequate for e2e triage.

## Design points (research.md D7)

- **Spread provisioning**: build from a **pinned upstream commit** (snapd CI
  pattern) rather than the `spread` snap — the snap is published from a
  personal namespace, and a pinned commit keeps supply chain in our control.
  The CI workflow caches the built binary.
- **Confined-e2e backend topology**: **(a) a minimal fake-adapter test snap**
  (`fake-snap/`) providing the `ubustt-socket` content slot with the session
  socket at `$SNAP_COMMON/run/ubustt.sock`. Topology (b) — in-VM
  `myna-server` with the socket placed in the shared dir — was rejected: the
  content share is only writable by the slot-side snap, so (b) cannot
  exercise the confined connect seam, which is the point of the suite.

## Desktop-session scope (spec acceptance 4)

Hotkey/IBus/indicator use-cases are **explicitly deferred**: a GNOME session
in a qemu guest on hosted runners is a significant additional cost (image,
session bootstrap, display). The confined e2e covers the client snap's
dictation path (`myna --socket ... --clip ...`) against the fake backend.
Desktop last-mile stays human acceptance + the Workshop env-gated suites.

## Plan

- `spread.yaml`: qemu backend, `ubuntu-24.04-64`.
- `tests/spread/confined-e2e/task.yaml`: install `myna` client snap +
  `myna-fake-backend` snap, `snap connect myna:backend
  myna-fake-backend:ubustt-socket`, drive a WAV-fixture dictation, assert the
  known fake-adapter transcript (FR-015).
- `.github/workflows/spread.yml`: nightly + path-filtered; builds the two
  snaps, builds spread from the pinned commit, runs the suite.
- Supersession: once the nightly is green for two weeks, the bespoke
  `snap.yml` smoke job shrinks to build-only (smoke assertions move to
  spread).

## Status

**Local VM validation: PASSED (2026-08-10).** The confined-e2e task ran
green on a clean qemu ubuntu-24.04-64 image: both snaps installed
`--dangerous`, `snap connect myna:backend
myna-fake-backend:ubustt-socket`, WAV-driven dictation against the confined
fake backend, scripted transcript asserted. Fixes the first runs surfaced:
fake-snap PYTHONPATH, guest-side snap path resolution, fixture staging under
confinement. The qemu image is a primed noble cloud image at
`~/.spread/qemu/ubuntu-24.04-64.img` (cloud-init seed: ubuntu/ubuntu +
password SSH); spread is built from the pinned commit, NOT the snap (the
snap has no kvm plug and cannot run the qemu backend with KVM).

First nightly CI run pending; revisit the verdict only if the hosted-KVM
assumption breaks.

**Update (2026-08-19, T53 validation).** adapter-smoke ran green locally for
the first time (whisper at modelctl v2.0.0-beta.12, batch + streaming). Three
latent bugs fixed to get there: (1) the `SPREAD_COMMIT` pin `052d7a1` no longer
exists upstream (history rewrite) - re-pinned to the `2026.07.12` tag's current
commit, so the CI build-spread step had been failing; (2) the task polled the
content-share socket path from the *host*, where it never exists - the share
lives only in the consuming snap's mount namespace, so the wait now targets
the backend's own `$SNAP_COMMON/run` and asserts the share once via
`snap run --shell myna.testbed`; (3) qemu's default CPU lacks x86-64-v2, which
numpy 2.x requires - the local build of spread now patches in `-cpu host`
(`dev/spread-build.sh`, applied by both local runs and CI). Local invocation
moved to `make spread` / `spread-smoke` / `spread-e2e` / `spread-debug`
(spread.yaml header).
