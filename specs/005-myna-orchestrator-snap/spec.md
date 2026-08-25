# Feature Specification: Myna orchestrator snap (dictation client packaging)

**Feature Branch**: `005-myna-orchestrator-snap`

**Created**: 2026-07-22

**Status**: Draft

**Input**: User description: "The orchestrator needs to ship as a snap. Evaluate the required changes and perform them, using the existing inference snaps as the pattern. Set up GitHub CI workflows for the snap build. Get to a working, locally verified state."

## Context

The Rust dictation client (`client/myna-desktop` + the `myna-dictate` testbed
CLI) is the shipped last mile of UbuSTT: it owns the microphone (audio-push
invariant), the hotkey, text injection, and the indicator publisher. Today it
only runs unsandboxed from a dev checkout. The inference snaps
(`whisper`/`nemotron`/`qwen`) serve the session API on a Unix socket under
their `$SNAP_COMMON`; a strictly-confined client cannot reach those paths
without an interface, and several client behaviors (IBus address discovery,
the control-socket path, shortcut installation) implicitly assume an
unconfined process. This feature packages the client as the `myna` snap and
makes the confined paths real, resolving the socket-exposure half of plan
T14c in favor of a writable content share.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Install the client as a snap and dictate (Priority: P1) 🎯 MVP

A user installs the `myna` snap next to a backend snap (e.g. `whisper`),
connects the documented interfaces, starts the daemon, triggers dictation
with the hotkey, and has the transcript typed into the focused application —
exactly the unsandboxed T21/T22 flow, but confined.

**Why this priority**: The entire point of the workstream; until the client
is confined and driving a confined backend, "UbuSTT ships as snaps" is only
half true (the half with no microphone).

**Independent Test**: On a GNOME/Wayland session with the `whisper` snap
installed: `snap install --dangerous myna_*.snap`, connect `pipewire` +
`backend`, run `myna`, trigger a session (portal hotkey, `myna.toggle`, or
`MYNA_ACTIVATION=stdin`), speak / play a clip, and observe injected text.

**Acceptance Scenarios**:

1. **Given** the myna snap is installed with `pipewire` and `backend`
   connected and a backend snap serving, **When** the daemon runs and a
   session is triggered, **Then** capture starts, PCM reaches the backend
   over the content-shared socket, and the transcript is injected via IBus.
2. **Given** `myna:backend` is not connected, **When** the user runs `myna`,
   **Then** it exits with an actionable message naming the `snap connect`
   command, not a bare IO error.
3. **Given** no `network` plug on the snap, **When** dictating, **Then** all
   traffic stays on Unix sockets / the session bus (offline invariant).

### User Story 2 - Confinement-correct desktop integration (Priority: P1)

Under confinement the client still: finds the IBus daemon's address file
(snapd redirects `$HOME`, the daemon writes under the real home), places its
control socket where AppArmor allows (`$XDG_RUNTIME_DIR/snap.myna/`), owns
`org.myna.Dictation` on the session bus (for the myna-shell indicator), and
activates via the GlobalShortcuts portal (which only serves packaged apps).

**Acceptance Scenarios**:

1. **Given** an IBus daemon running in the session, **When** the confined
   daemon starts, **Then** injection connects (no "no IBus socket dir"
   error) — verified live, with the discovery covered by unit tests.
2. **Given** the daemon running with `--dbus`, **When** a client reads the
   session bus, **Then** `org.myna.Dictation` is owned and state/level
   properties update during a session (the feature-004 contract, confined).
3. **Given** control-socket activation (`MYNA_ACTIVATION=control`), **When**
   `myna.toggle` runs, **Then** it reaches the daemon's control socket under
   `$XDG_RUNTIME_DIR/snap.myna/`.

### User Story 3 - Backend snaps expose their socket to confined clients (Priority: P1)

Each inference snap exposes `$SNAP_COMMON/run` (where `socket.path` defaults)
as a writable content slot `ubustt-socket`, so the client snap reaches the
socket through a bind mount with the AppArmor rw rules snapd adds for
writable shares (the named-socket case in snapd's content interface).

**Acceptance Scenarios**:

1. **Given** the rebuilt `whisper` snap installed and serving, **When**
   `myna:backend` connects to `myna-whisper:ubustt-socket`, **Then**
   `$SNAP_DATA/backend/run/ubustt.sock` is the live session socket and a session
   round-trips.
2. **Given** any of whisper/nemotron/qwen providing the slot, **When** the
   plug is moved between them, **Then** the same client path works unchanged.

### User Story 4 - CI builds the snap (Priority: P2)

Every push/PR that touches the client or the snap packaging builds the
`myna` snap in GitHub Actions and smoke-tests the artifact (install
`--dangerous`, `--help` runs confined), so packaging breakage is caught in
CI, not on a developer machine.

**Acceptance Scenarios**:

1. **Given** a PR touching `client/**` or `myna-snap/**`, **When** CI runs,
   **Then** the snap builds and the artifact is uploaded.
2. **Given** the built artifact, **When** the smoke job installs it,
   **Then** `myna --help` and `myna.testbed --help` exit 0 under
   confinement.

### Edge Cases

- Backend plug connected before the backend server has ever run: the share's
  `run/` dir may not exist yet; the daemon's "no backend socket" message
  covers it (retry once the backend daemon has started).
- `myna:pipewire` not connected (snapd denies auto-connection by design):
  capture fails with the orchestrator's existing `capture_failed` surface;
  the README documents the manual connect.
- Portal unavailable/rejected (non-GNOME session): `MYNA_ACTIVATION=control`
  + `myna.install-shortcut` is the documented fallback.
- Refresh: nothing in the snap writes revisioned paths into user config
  (shortcut commands use `/snap/bin/myna.toggle`).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The `myna` snap MUST build from `client/` with snapcraft
  (core24, rust plugin, gnome extension for the GTK indicator) and ship
  `myna-desktop` (daemon) and `myna-dictate` (`myna.testbed`).
- **FR-002**: The snap MUST NOT plug `network` (offline invariant); all
  boundaries are Unix sockets / session D-Bus / PipeWire.
- **FR-003**: The daemon app MUST plug `pipewire` (capture), `desktop`
  (portals + notifications), `desktop-legacy` (IBus private socket),
  `wayland`/`x11`/`gsettings`, and the `backend` content plug.
- **FR-004**: The snap MUST slot `org.myna.Dictation` on the session bus so
  the confined daemon can serve the feature-004 contract.
- **FR-005**: Each inference snap MUST expose `$SNAP_COMMON/run` as a
  writable content slot `ubustt-socket`; the client plug targets
  `$SNAP_DATA/backend`.
- **FR-006**: IBus address discovery MUST search the invoking user's real
  home (from `/etc/passwd`) in addition to `$XDG_CONFIG_HOME`/`$HOME` when
  running confined (`$SNAP` set).
- **FR-007**: The control-socket default path MUST be writable when
  confined. (Satisfied by snapd itself: `$XDG_RUNTIME_DIR` is scoped to
  `/run/user/<uid>/snap.<instance>` — verified live; no client change
  needed.)
- **FR-008**: Shortcut installation MUST NOT write revisioned paths; the
  snap path installs via `dconf` (no schema visibility inside the base).
- **FR-009**: A CI workflow MUST build the snap on relevant changes and
  smoke-test the artifact.
- **FR-010**: All existing client tests MUST stay green; new confined-path
  logic MUST land with unit tests (constitution I).

### Key Entities

- **`myna` snap**: the client; apps `myna` (daemon), `myna.toggle`,
  `myna.install-shortcut`, `myna.testbed`.
- **`backend` content share**: slot `$SNAP_COMMON/run` (backend) → plug
  `$SNAP_DATA/backend` (client); carries `ubustt.sock`.
- **Activation modes**: `portal` (default, packaged path), `control`
  (control socket + `myna.toggle`), `stdin` (debug).

## Success Criteria *(mandatory)*

- **SC-001**: `snapcraft pack` produces `myna_0.1.0-dev_amd64.snap`; it
  installs `--dangerous` with strict confinement.
- **SC-002**: With `myna:pipewire` and `myna:backend ← myna-whisper:ubustt-socket`
  connected, a confined session against the whisper snap completes end to
  end (testbed WAV clip round-trip; daemon trigger path exercised).
- **SC-003**: `org.myna.Dictation` is owned and updates live while the
  confined daemon runs `--dbus`.
- **SC-004**: Full workspace `cargo test` + `clippy -D warnings` green.
- **SC-005**: The CI snap workflow is green on this branch.
- **SC-006**: No `network` plug in the built snap's manifest.
