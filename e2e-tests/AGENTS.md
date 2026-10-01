# Preface

Read this before changing the Myna Settings end-to-end tests. Read the
top-level `.kb/agents.md` before continuing below.

# Overview

This directory runs the real Myna Settings binary through its user flows on
fresh per-series GNOME desktops: onboarding, backend switching, model
settings, and later the shell extension, the shortcut portal and spoken
dictation. A suite declares the machine state it starts from
(`# snapshot: bare|components-only|installed`), `run-suite.sh` reverts a
libvirt snapshot to get exactly that state, and the suite drives the app
through AT-SPI while asserting machine state over SSH.

# Important

- `tools/` is ported from `~/myna-onboarding-stable/tools` and
  `~/myna-config-ux-round3/tools`; those are prototypes, this is the
  maintained copy. Each file's header says what changed in the port. Do not
  "improve" the ported driver semantics without checking the prototype's
  notes (`~/myna-onboarding-stable/notes/harness.md`): most oddities there
  are measured toolkit behavior, not accidents.
- Assert over SSH (snapd, gsettings, systemctl, dconf), never over pixels.
  AT-SPI waits (`waitfor:`/`gone:`) gate the flow; `wait:N` is for settling
  after an action, and a suite that needs a long one is probably missing a
  waitable condition.
- Polkit outcomes come from the temporary rules and `cancel-agent.py`
  (`--polkit`, `--apply-polkit`), not from typing into dialogs. One suite
  may prove the real dialog surfaces; every other suite must be
  deterministic.
- The VM's polkit rules files (`49-myna-shot*.rules`) must never survive a
  run; `shot-remote.sh` removes them on EXIT. A suite that kills the remote
  shell leaves the VM answering its own prompts - reprovision or remove the
  files before trusting the next run.
- Accessible names are translated strings. Suites run under the default
  `LANGUAGE`; a copy change in the app breaks selectors here by design, and
  the fix is the selector, not the string.
- Snapshots are disk-only with the VM shut off. Never take a memory-state
  snapshot: several gigabytes each and the session must prove it comes up
  from boot anyway.
- The myna-config binary is built in the `myna-noble` workshop (GTK 4.14
  floor) and runs on every series. That is deliberate: one build, three
  toolkits, and Noble's bugs cannot hide behind a newer build host.

# CI phases

Phase 1 (current): local libvirt VMs, `run-suite.sh`. Phase 2: GitHub
Actions, authd-style - per-series qcow2 cached in actions/cache and pushed
to ghcr via oras, per-series deb build, nightly matrix
noble/resolute/stonking, PR runs gated by an `e2e-tests` label. Phase 3:
flavor-B suites on the seat session (extension, portal shortcut, spoken
dictation with `speak.sh`'s virtual mic).
