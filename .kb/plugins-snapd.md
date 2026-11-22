# Preface

What snapd 2.78 does when an unprivileged app installs, connects and removes model components of `myna-text` and `myna-translation`, as observed on a throwaway box (not the laptop's snapd), and what stays unproven because both snaps are sideloaded. The inputs to Settings' plugin flow.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Observed 2026-11-22 on snapd 2.78+ubuntu27.04, user `probe` (no groups) talking to `/run/snapd.socket`, polkit 128 with a rule that logged and granted `io.snapcraft.snapd.*` (no agent, so no prompt). The store has neither text snap registered, so REST component installs were exercised on `test-snap-with-components` (16 KB components, same code path); sideloaded snaps were exercised with `snap install --dangerous`. Probe scripts: `~/myna-processing-graphs/bench/plugins/`.

# Important

- **One polkit action per request.** Every write (install, remove, connect, sideload) asks `io.snapcraft.snapd.manage` once. Snap plus components in one POST is one authorization, so one prompt. Without an agent the answer is HTTP 401 `login-required` (snapd log: "polkit error: Authorization requires interaction"). Send `X-Allow-Interaction: true` for an agent to be consulted.
- **REST component install only works for a store snap.** For a sideloaded snap, installed or not, `POST /v2/snaps/myna-text {"action":"install","components":[...]}` fails: 400 "cannot install components for a snap that is unknown to the store" (not installed and unregistered: 404 `snap-not-found`; component already there: 400 `snap-already-installed`). Until `plugin-store-enablement`, a component of these snaps can only be installed by sideloading the `.comp` (multipart `POST /v2/snaps`, what `snap install --dangerous x.comp` sends, also one `manage` authorization). The store half is proven only on another snap.
- **Omitting `components` (or sending `[]`) installs every component of a store snap.** `myna-parakeet` and `myna-nemotron` pulled 4.8 GB and 1.6 GB into one change. Always name the components.
- **No auto-connect for sideloaded snaps.** Content connections between sideloaded snaps (and store `myna` with sideloaded text snaps) stay `manual`; installing `myna` after, or refreshing a text snap, connected nothing. Settings or the installer must `snap connect myna:backend <snap>:provider` for each text snap. `myna:backend` allows several slots at once (`myna-text` plus `myna-translation`, connected one after the other). Removing a snap drops its connection; a sideload refresh keeps it.
- **Store same-publisher auto-connect is real for one provider.** Store `myna` already installed, backend unconnected, then store `myna-nemotron` (same publisher) installed: `myna:backend` was connected to `myna-nemotron:provider` without a manual step (aborted before the 800 MB components finished). Unproven: a second store provider beside a connected first, and `myna-text` / `myna-translation` from the store (not registered).

# Architecture

## Component install and remove (Q1, Q6)

Store snap `test-snap-with-components`, REST as `probe`, all 202 + change:

- Not installed, `{"action":"install","components":["one"]}`: one change installing snap and component (about 3.5 s).
- Installed, `components:["two","three"]`: components only, one change, one authorization.
- `{"action":"remove","components":[...]}`: one change per request, several components in one request fine, 0.6 to 1 s; the snap stays.
- Unknown component: 400 "cannot find component ... in snap resources", still one authorization.

## Running daemons (Q3, Q4)

Removing and sideloading a component under a running daemon (15 cycles, a 10 ms `capabilities` poller on the share socket, plus a ticker snap on the same content plug), `myna-text` and `myna-translation`:

- Zero failed calls, one call slower than 30 ms (31.8 ms). The daemon is not frozen and not restarted: unlike a revision refresh (`snapd-refresh-freezes-snap-processes`, 150 to 190 ms), a component change shows no stall above the box's idle noise (a quiet ticker alone logged a 41 ms stall in 90 s; stalls of 55 to 70 ms appeared in 3 of 30 windows and in the quiet run too, load average 6).
- The ticker snap on the plug (stand-in for the `myna` daemon) is unaffected by that same noise.
- `capabilities` reflects the change with no daemon restart or rescan trigger: the model set changed at the link/unlink step, 0.09 to 0.18 s before a sideload install returned and 0.6 to 0.8 s before a REST remove returned (the change's last task, `setup-profiles`, is still running). So when snapd reports Done the model is already visible (after remove: `models: []`).

## Metadata (Q5)

- `GET /v2/find?name=myna-parakeet` and `snap info` list no components. The store info API does, with `fields=resources` (`name`, `revision`, `type`, `download.size`): `myna-parakeet` lists `gpu-nvidia` 1,557,049,344 B, `model-parakeet-fp32` 2,549,030,912 B, `model-parakeet-int8` 729,399,296 B. snapd 2.78 does not surface them through `/v2/find`, so a Settings catalog needs the sizes from its own catalog or the store API.
- `GET /v2/snaps/<name>` (installed) lists every component the revision declares, installed or not, with `summary`, `description`, `type`; `revision` is `"unset"` and there is no `installed-size` / `install-date` for the ones not installed. No download sizes.
- `myna-text` and `myna-translation` are not registered: `/v2/find` answers 404 `snap-not-found`.
