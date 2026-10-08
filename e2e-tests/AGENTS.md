# Preface

Read this before changing the Myna Settings end-to-end tests. Read the
top-level `.kb/agents.md` file before continuing below.

# Overview

A suite runs on the host: `run-suite.sh` restores the LXD snapshot it
declares, pushes the binary and `tools/` into the VM, and the suite calls
`shot` (the app under Xvfb on the autologin user's session bus, driven by
`shot-driver.py` steps) and asserts with `assert_on` inside the VM. CI runs
the same `make e2e` per series and desktop (`.github/workflows/e2e.yml`).

The desktop is an axis, `E2E_DESKTOP=gnome|xubuntu` (`--desktop` on the
scripts). Its cloud-init is `vm/user-data.common` merged with
`vm/user-data.<desktop>` by `lib.sh`; session readiness (`session_up`) and
the X11 environment `on_vm` hands out are per desktop. Xubuntu is Xfce on
X11 (lightdm autologin).

# Important

- The GNOME VM keeps the name `myna-e2e-<release>`; other desktops are
  `myna-e2e-<desktop>-<release>`, so they coexist. A new desktop adds a
  `user-data.<desktop>`, a `session_up` arm and, if X11, an `on_vm` arm in
  `lib.sh`; shared packages go in `user-data.common`.
- Xubuntu is informational in CI (`continue-on-error`) until its suites
  pass; scheduled and dispatch runs only, noble only.

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
- Snapshots are taken stopped. A suite that needs a new machine state gets a
  new provisioning stage, not setup in the suite.
- `tools/` descends from the prototypes in `~/myna-onboarding-stable/tools`;
  driver oddities there (`harness.md`) are measured toolkit behaviour.
