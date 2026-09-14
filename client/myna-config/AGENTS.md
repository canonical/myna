# Preface

Read this file before changing Myna Settings: the GTK application that onboards a machine, switches and configures inference backends, and edits the dictation settings the daemon reads live.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Myna Settings is a host application, not a snap. It talks to snapd on the user's behalf and escalates to root through polkit for the few `snap` commands that need it, so it is packaged as a deb from `myna-config-deb/` and only shares the `myna-core` crate with the confined client. Everything the user can trigger is expressed as a plan of exact `snap` commands first, shown to the user, and only then executed.

# Important

- Two privilege paths exist and they are not interchangeable. Backend switching (`connect`, `disconnect`, `restart myna.myna`) goes straight to `/run/snapd.socket` as the user and snapd asks polkit itself. Backend configuration (`snap run <backend>.modelctl ...`) runs as root through one `pkexec myna-config --apply-plan` invocation. Do not route a new operation through `pkexec` when snapd's REST API can do it unprivileged.
- The `--apply-plan` executor is the trust boundary. It accepts only operations matching the exact shapes the UI produces (`apply_plan.rs`, `system_configurator.rs`). Extend the whitelist deliberately and with a test; never pass free-form argv through it.
- Nothing runs through a shell. Build every subprocess as a `CommandRequest` and run it through the `CommandRunner` port so tests can substitute a fixture.
- Subprocess spawning is budgeted per refresh reason (`docs/refresh-budget.md`). A new `snap` read must fit the budget or change it explicitly.
- Domain and controller modules are GTK-free and tested headlessly. Keep GTK to `ui/`, `*_ui.rs`, and `app.rs`.
- Every user-visible string goes through gettext. Adding or changing one requires `make i18n` and committing the template; `make check` fails while it drifts.
- Strict confinement was measured and rejected (`docs/confinement.md`). Do not reopen it without new evidence.
- The GSettings schema this application writes is owned by `client/data/` and shared with the daemon.

# Architecture

Hexagonal. `ports.rs` declares the traits the application depends on (backend repository, system configurator, snap installer, client settings). `adapters/` implements them against real snapd, `snap`, `pkexec`, and Gio. `domain.rs`, `active_backend.rs`, `backend_apply.rs`, `onboarding.rs`, and `machine.rs` hold the pure decision logic. The `*_controller.rs` and `*_ui.rs` pairs bind that logic to GTK, and `operation_gate.rs` ensures one privileged operation runs at a time.

# Directory

- `src/adapters/` - snapd REST client, `snap` CLI repository, pkexec configurator, Gio settings.
- `src/ui/` - One module per Blueprint template in `data/`.
- `src/bin/` - Test fixture that stands in for a real command runner.
- `data/` - Blueprint templates, CSS, desktop entry, man page, gresource manifest.
- `docs/` - Decision records: confinement gate, onboarding flow, refresh budget.
- `po/` - gettext template and translator instructions.
- `tests/` - Contract tests per port and adapter. `snap_packaging.rs` covers the `myna.config` gsettings wrapper the snap ships, which is a shell script and not this application.
