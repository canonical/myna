# Tasks: Myna orchestrator snap

**Input**: [spec.md](spec.md), [plan.md](plan.md)
**Prerequisites**: inference snaps installed; snapcraft 9 + LXD on the host

## Phase 1: Confinement fixes in the client (TDD)

- [x] T001 [P] IBus address discovery searches the real home under
  confinement (`/etc/passwd` lookup; candidates across all config roots) —
  `client/myna-desktop/src/inject/ibus.rs` + unit tests
  (`real_home_parsed_from_passwd`, `address_found_in_later_candidate_dir`,
  `stale_daemon_is_not_picked`)
- [x] T002 [P] Control-socket path under confinement — turned out to need
  **no code change**: snapd already scopes `$XDG_RUNTIME_DIR` to
  `/run/user/<uid>/snap.<instance>` (writable), so the default path is
  legal as-is; `control.rs` gained the doc comment recording why
  (`client/myna-desktop/src/shortcut/control.rs`)
- [x] T003 [P] `--install-shortcut` writes `/snap/bin/<instance>.toggle`
  (never a revisioned exe path) — `client/myna-desktop/src/bin/myna-desktop.rs`
- [x] T004 Workspace `cargo test` + `clippy --all-targets` green (SC-004)

## Phase 2: The myna snap

- [x] T005 `myna-snap/snap/snapcraft.yaml`: rust plugin (gnome extension),
  apps `myna`/`toggle`/`install-shortcut`/`testbed`, `backend` content plug,
  `org.myna.Dictation` dbus slot, no `network`
- [x] T006 `scripts/myna-daemon` launcher (portal + `--dbus` defaults,
  actionable no-socket error) + `scripts/myna-install-shortcut` (dconf)
- [x] T007 `dev/prepare.sh` client staging + `.gitignore`
- [x] T008 `snapcraft pack` green; manifest inspected (plugs/slot/no-network)
  — plus three confinement-driven packaging fixes: no `gnome` extension
  (core22 SDK glibc breaks noble build tools), rustup toolchain via a nil
  `rust-deps` part (rust-plugin validation is chicken-and-egg with rustup
  1.29), staged pipewire module/config set + SPA/module/config env
- [x] T009 `snap install --dangerous`; `myna --help` / `myna.testbed --help`
  run confined

## Phase 3: Backend socket exposure (T14c, content-share half)

- [x] T010 [P] `ubustt-socket` writable content slot in whisper/nemotron/qwen
  snapcraft.yaml
- [x] T011 Rebuild + reinstall `whisper` with the slot; connect
  `myna:backend`; socket visible at `$SNAP_DATA/backend/run/ubustt.sock`
- [x] T012 Confined end-to-end: `myna.testbed --clip` round-trip through the
  shared socket — exact LibriSpeech transcript via whisper-small (SC-002)
- [x] T013 Daemon live checks (all confined, final artifact): `--dbus` name
  owned; control-mode `myna.toggle` cycle drives idle → **recording** (live
  mic RMS/peak on the bus) → finalize → idle, no error; portal **bind**
  succeeds (literal Super+D press needs a human — no uinput access). SC-003
  met. Human acceptance left: hotkey press + spoken injection.

## Phase 4: CI + docs

- [x] T014 `.github/workflows/snap.yml`: build the snap on client/packaging
  changes, upload artifact, confined smoke test
- [x] T015 spec/plan/tasks artifacts (this directory)
- [x] T016 `myna-snap/README.md` (build/install/connect/verify)
- [x] T016b whisper/nemotron/qwen README notes for the slot
- [ ] T017 plan tracker row (T57) + CLAUDE.md state + design-note T14c update
  (design note done; plan row + CLAUDE.md pending)

## Field findings (2026-07-24, user acceptance session)

- **Stale session-bus guid**: shells/tmux surviving logout carry
  `DBUS_SESSION_BUS_ADDRESS` with the old bus's `guid=`. zbus rejects it
  (fixed: shared stale-guid-tolerant `connect_session()` now used by the
  portal bind too); sd-bus (busctl) cripples the connection (EPERM /
  AccessDenied); GIO ignores it. User-facing fix: unset/correct the env var.
- **Confined signal mediation**: snapd's dbus slot admits unconfined peers
  for method calls/replies and AppArmor-denies *custom-interface* signal
  broadcasts to unconfined subscribers — but its permanent policy allows
  `org.freedesktop.DBus.Properties` sends on the snap's own path with
  `peer=(name=org.freedesktop.DBus)`, exactly the shape dbus-daemon assigns a
  destination-less broadcast. So standard `PropertiesChanged` crosses
  confinement: the publisher is properties-only (no custom signals) and the
  extension subscribes to `g-properties-changed` — no polling, no signal gap
  (contract 004 dbus-interface.md §Confinement).
- **Portal v1 UX**: bind sheet re-prompts every daemon start (no persist
  tokens in v1 / ashpd 0.13); once a key is confirmed in the sheet the grab
  works (user-verified 2026-07-24 — the earlier ESC-t report was pre-fix).
  Activation default flipped to `control` (`myna.toggle` +
  `myna.install-shortcut`); portal is opt-in (`MYNA_ACTIVATION=portal`).
- **Press-to-toggle is the default activation style everywhere** (user
  decision 2026-07-24: hold-to-talk is uncomfortable): portal trigger gains
  `ActivationMode` (Toggle default, `--hold` opts into hold-to-talk) with
  hermetic Dedup tests; control socket was always toggle.
- **`myna.install-shortcut` bug fixed**: double slash in the dconf key
  (`myna//name`) + sed delimiter vs slash-containing paths.
- Control-mode end-to-end accepted by the user (toggle → recording →
  injected transcript).

## Notes

- T011–T013 need the local machine (snapd + session); they are the SC-002/003
  evidence recorded in the PR/task notes.
- nemotron/qwen rebuilds deferred (T010 yamls only); whisper is the verified
  reference.
