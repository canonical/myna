# Myna Settings strict-snap confinement gate

**Decision date:** 2026-09-05
**Target tested:** Ubuntu 26.10 development, snapd 2.76.3, Snapcraft 9.0.1,
`core24`, AppArmor enabled
**Status:** T014 corrected evidence and contract complete; full strict
integration is **blocked**

## Result

The strict probe measured that all six snapd REST attempts failed before an
HTTP response. No correlated AppArmor `DENIED` record was available from
`journalctl -k` for either run, so **the measured claim is only pre-HTTP
transport denial**. AppArmor attribution is a source-backed inference:
snapd's interface policy grants `/run/snapd.socket rw` through
`snapd-control`, while `desktop`, `wayland`, `x11`, the backend `content` plug,
and the snap-local `polkit` interface do not grant it. Polkit can authorize a
request only after transport succeeds; it does not add confinement permission.

`snapd-control` is deliberately rejected. It is super-privileged,
non-auto-connected, Store-reviewed, and grants substantially more management
authority than Settings needs. There is no package-management XDG portal.

No viable shipping boundary is demonstrated today. The preferred candidate is
a **separately installed host mediator** with the narrow contract below, but a
strict snap also needs an approved interface/portal policy to reach that host
service. A host daemon alone does not bypass AppArmor. Shipping is therefore
blocked on Ubuntu Desktop/platform engineering owning and packaging the
mediator **and** the snapd/Store team approving a least-privilege client
transport (or providing an equivalent purpose-built portal/interface). This is
an explicit blocked gate, not a claim that strict integration or a production
mediator exists.

## Host-app runtime today vs. strict shipping tomorrow

The `myna-config` GUI that runs today is a **host application**, not a strict
snap, and everything below in this document about `snapd-control`, portals,
and the mediator is about the still-blocked strict-snap shipping path. The
runtime distinction matters for reviewers and users:

- Today the host GUI opens `/run/snapd.socket` directly, without a helper
  binary and without `pkexec` wrapping it. Snapd receives the request from
  an unconfined user process, sees the `X-Allow-Interaction: true` header,
  and asks polkit for a normal per-action prompt. This is the same
  authorization mechanism a shell `snap connect` invocation would trigger;
  no elevated child, no root helper, no bespoke privilege escalation.
- The `snapd-control` exception granted to the Software / App Center is
  *not* a template for Myna. It is a Store-reviewed, super-privileged
  interface reserved for a small allowlisted set of first-party stores.
  Myna does not, and must not, request it; the current host-app runtime
  therefore relies on being outside strict confinement, not on any privileged
  interface.
- Shipping `myna-config` as a **strict snap** remains blocked exactly for the
  reasons documented below: strict confinement forbids opening the snapd
  socket without a granted interface, no acceptable interface exists, and
  no snap-management portal exists. The mediator contract in
  [Mediator contract v1](#mediator-contract-v1) is the future design for
  that shipping path, not the runtime behaviour of the current host GUI.

## Reproducible evidence

The disposable probe is in `config-ui/confinement-probe/`. It deliberately does
not modify `myna-snap/snap/snapcraft.yaml`, request `snapd-control` or `polkit`,
or execute host `snap`, `snapctl`, `pkexec`, or another snap's `modelctl`.

Build and install on the target image:

```sh
cd config-ui/confinement-probe
snapcraft pack
sudo snap install --dangerous ./myna-confinement-probe_1_amd64.snap
sudo snap connect myna-confinement-probe:backend myna-parakeet:ubustt-socket
myna-confinement-probe.probe
sudo snap remove --purge myna-confinement-probe
```

The write probes use the syntactically invalid snap name `invalid!snap`, which
can never identify an installed snap, so even an unexpectedly permitted
request cannot change host state. Keep any polkit dialog visible while running
the probe. No dialog was observed, which is consistent with the request
failing before snapd/polkit, but does not identify the kernel policy that
caused the failure.

| Surface | Reproduction | Result | Evidence class |
|---|---|---|---|
| Private GSettings | probe `get`, `set`, `reset` of its own keyfile schema | `before='' after='t014' reset=''` in `$SNAP_USER_COMMON` | measured 2026-09-05 |
| Snap namespace | probe and host `readlink /proc/self/ns/mnt` | host `mnt:[4026531832]`; probe `mnt:[4026533656]` | measured 2026-09-05 |
| snapd discovery/read | host and strict-probe `curl --unix-socket /run/snapd.socket` for system info, connections, and backend config | host returned HTTP 200; every strict request failed before HTTP with curl exit 7 | measured pre-HTTP transport denial, 2026-09-05 |
| Cross-snap data | connect probe `backend` content plug, then `find "$SNAP_DATA/backend"` | only `backend/run/ubustt.sock` was exposed; no modelctl config/schema data or executable | measured 2026-09-05 |
| Connect/disconnect | probe `POST /v2/interfaces` with `X-Allow-Interaction: true` and nonexistent endpoint | both failed before HTTP with curl exit 7; no state change | measured pre-HTTP transport denial, 2026-09-05 |
| `snap set` equivalent | probe `PUT /v2/snaps/.../conf` with `X-Allow-Interaction: true` and nonexistent snap | failed before HTTP with curl exit 7; no state change | measured pre-HTTP transport denial, 2026-09-05 |
| Authorization UX | same explicit write probes | no dialog appeared; request did not reach HTTP | measured observation, 2026-09-05 |
| Host executables | probe contains no host command execution | intentionally not attempted; a visible `/usr/bin/snap` path is not authority to execute it | measured policy invariant |

Host commands and abbreviated outputs:

```text
$ snap version
snap 2.76.3; snapd 2.76.3; ubuntu 26.10; amd64
$ stat -Lc '%n mode=%a owner=%U group=%G type=%F' /run/snapd{,-snap}.socket
/run/snapd.socket mode=666 owner=root group=root type=socket
/run/snapd-snap.socket mode=666 owner=root group=root type=socket
$ curl --unix-socket /run/snapd.socket http://localhost/v2/system-info
type=sync status-code=200 version=2.76.3 series=16 confinement=strict
$ myna-confinement-probe.probe
probe-version=1 snap=myna-confinement-probe revision=x1
mount-namespace=mnt:[4026533656]
snapd-socket=mode=666 owner=root group=root type=socket
private-gsettings: before='' after='t014' reset=''
# content-share lists only backend/run/ubustt.sock
# all six HTTP probes report curl-exit=7 before an HTTP response
```

The exact sanitized output, host versions, reproduction commands, and audit
query are checked in at
[`confinement-probe/results/ubuntu-26.10-snapd-2.76.3.txt`](confinement-probe/results/ubuntu-26.10-snapd-2.76.3.txt).
`snapcraft pack` succeeded, the resulting snap was installed with
`--dangerous`, its content plug was connected, and the installed strict probe
ran. All six REST attempts (including the interactive header on writes) failed
with curl exit 7 despite the mode-0666 socket being visible. This proves only
pre-HTTP transport denial. The AppArmor explanation below is source-backed
inference, not measured audit proof.

## Authoritative sources

Sources were checked on 2026-09-05. The target recipe assumes snapd 2.75; the
host evidence used 2.76.3. Source links are pinned to snapd commit
`931c5f6c045ff7bb3f620efff34b4473f3c8f81f` (snapd 2.77 development source at
review time), rather than relying on moving `master`.

- The official [REST API guide](https://snapcraft.io/docs/how-to-guides/snap-development/use-the-rest-api/)
  identifies `/run/snapd.socket`, access levels, asynchronous change IDs, and
  `X-Allow-Interaction: true` for polkit.
- snapd's pinned [`snapd-control` policy](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/interfaces/builtin/snapd_control.go#L27-L48)
  is the rule granting `/run/snapd.socket rw`; its base declaration denies
  installation and auto-connection. Official
  [Store/interface guidance](https://snapcraft.io/docs/reference/interfaces/snapd-control-interface/)
  calls it super-privileged and says approval is limited to strict,
  specific circumstances, primarily brand-store owners.
- snapd's pinned [access checks](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/daemon/access.go#L102-L190)
  distinguish the main and snap sockets. The official
  [architecture reference](https://snapcraft.io/docs/reference/system-architecture/)
  assigns `/run/snapd.socket` to `snap` and `/run/snapd-snap.socket` to
  `snapctl`; the latter is not an unrestricted substitute.
- snapd's pinned [polkit path](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/daemon/access.go#L39-L62)
  uses socket peer PID/UID and permits interaction only when requested. The
  snap [`polkit` interface](https://snapcraft.io/docs/reference/interfaces/polkit-interface/)
  instead lets a snap daemon publish/check its own actions; it is
  super-privileged and non-auto-connected, and does not open snapd's socket.
- [`desktop`](https://snapcraft.io/docs/reference/interfaces/desktop-interface/)
  grants basic desktop resources, not snap management. Strict confinement
  requires an explicit interface for host resources
  ([confinement reference](https://snapcraft.io/docs/explanation/security/snap-confinement/)).
- The [XDG Desktop Portal API](https://flatpak.github.io/xdg-desktop-portal/docs/api-reference.html)
  has file, URI, print, settings, notification, capture, and related desktop
  APIs, but no snap/package-management API. `OpenURI` can hand a store URI to a
  host application; it cannot discover, configure, connect, or monitor snaps.
  The interface inventory was also checked in
  [xdg-desktop-portal source at commit `86bd3e2` (2026-09-04)](https://github.com/flatpak/xdg-desktop-portal/blob/86bd3e26eabb7750ebb6f804e1e3ec122608a647/data/meson.build#L9-L41).
- The [`content` interface](https://snapcraft.io/docs/reference/interfaces/content-interface/)
  may share files, executables, data, or sockets. Its content identifier is a
  compatibility contract and snapd does not synchronize producer/consumer
  updates. Sharing an executable would still run it under the consumer's
  confinement, so it is not a host-execution escape.
- Endpoint contracts are pinned for
  [`GET /v2/system-info`](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/docs/api/v2/paths/system-info.yaml),
  [`GET /v2/connections`](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/docs/api/v2/paths/connections.yaml),
  [`GET/PUT /v2/snaps/{name}/conf`](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/docs/api/v2/paths/snaps-name-conf.yaml),
  [`POST /v2/interfaces`](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/docs/api/v2/paths/interfaces.yaml),
  and [`GET /v2/changes/{id}`](https://github.com/canonical/snapd/blob/931c5f6c045ff7bb3f620efff34b4473f3c8f81f/docs/api/v2/paths/changes-id.yaml).

## Rejected boundaries

1. **Direct snapd REST from the strict GUI:** blocked without socket policy.
   Polkit is authorization, not confinement permission.
2. **`snapd-control`:** excessive authority and exceptional Store policy. Myna
   must never request it.
3. **Snap `polkit` interface:** concerns a snap's own daemon/actions, adds
   super-privileged review, and does not grant snapd socket access.
4. **Portal:** no snap-management portal exists.
5. **Content-shared/host executable:** content sharing does not confer host
   authority. Spawning host `snap`, `pkexec`, or another snap's `modelctl` is
   forbidden, brittle, and not a shipping boundary.
6. **Transcription socket as control plane:** it is intentionally unprivileged;
   adding host mutation would create a privilege-escalation surface.
7. **Host mediator without a client interface:** not reachable merely because
   it is installed outside the snap. A dedicated reviewed D-Bus/socket policy
   or portal is part of the named platform dependency above.

## Mediator contract v1

The mediator is a separately packaged host component, not part of the snap and
not implemented by T014. Its transport may be a narrowly permissioned Unix
socket or system D-Bus service selected during platform review. It must never
offer arbitrary argv, path access, shell execution, generic snapd forwarding,
or arbitrary snap names.

### Identity and authorization

- The root-owned host service uses transport credentials for UID/PID and
  validates the exact allowlisted snap AppArmor security label using
  `SO_PEERSEC` for UDS or the D-Bus sender's `LinuxSecurityLabel`. Credentials
  alone do not prove snap identity; checks must be tied to the accepted
  connection to avoid PID reuse. It also validates allowlisted Myna/backend
  snap identities and plug/slot names.
- The mediator owns caller-bound authorization. Before a mutation it asks
  polkit about the original peer subject using narrow mediator actions
  (`configure-backend` or `switch-backend`), with user interaction allowed only
  for a visible user action. It never treats its own root identity as proof
  that the caller is authorized. Background refresh never prompts.
- After its own authorization succeeds, the mediator talks to snapd as root.
  Snapd therefore authorizes the mediator, not the GUI caller; the mediator's
  peer check and polkit policy are mandatory security boundaries. The GUI
  never handles macaroons, polkit agents, root credentials, or raw snapd
  responses.

### Envelope and negotiation

Negotiation is connection/session state, not merely an optional first request.
Every non-negotiation operation is rejected with `negotiation_required` until
the same authenticated transport session has successfully negotiated.
Mutation submissions contain:

```text
protocol: {major: 1, minor: 0}
idempotency_key: opaque collision-resistant value (required for mutations)
operation: one typed operation below
```

`Negotiate{supported:[{major,minor}]}` returns
`Negotiated{selected, mediator_version, snapd_version, capabilities[]}`.
Unknown major versions fail `incompatible_protocol`; newer minor fields are
ignored. Capability names, not version guesses, gate optional operations.
The mediator, never the caller, generates an unguessable operation ID after
validation and returns it from mutation acceptance. Read requests do not take
operation IDs.

Typed operations and successful results:

| Request | Result |
|---|---|
| `ListInstalled{}` | `{snaps:[{backend_id, version}]}` |
| `DiscoverBackends{}` | `{backends[], connections[], active_backend?}` |
| `ReadBackend{backend_id}` | `{config, status, models, engines, revisions}` |
| `ApplyBackendConfig{backend_id, expected_revision, steps[]}` | `{operation_id}` |
| `SwitchBackend{from[], to, expected_connections_revision}` | `{operation_id}` |
| `GetOperation{operation_id}` | `{state, progress?, result?, error?}` |
| `CancelOperation{operation_id}` | authoritative `{state, result?, error?}` |

Backend IDs are selected from an immutable platform-owned list of snap IDs.
Read requests name exact allowlisted keys; the mediator never uses an
unfiltered config GET. Writable keys and tagged value constraints (`string`,
`boolean`, `integer`, `enum`) are compiled into the mediator package for each
supported backend contract. Socket paths, secrets, unknown keys, arbitrary
snap names, and arbitrary dotted keys are rejected. Allowed keys preserve
their defined scope and dotted spelling. The adapter caches opaque revisions
from its latest `BackendSnapshot`/`ConnectionSnapshot` read and attaches them
when mapping the existing apply/switch plans; a stale or missing cache returns
`conflict` and forces refresh. Thus revisions do not enter domain or UI types.
Mutations are compare-and-set against those revisions. `steps` is a canonical
ordered list of typed `SnapSet`, `UseModel`, `UseEngine`, and `Restart`
effects. Configuration keys are unique and carry tagged values; duplicate keys
are invalid. `Restart` includes `backend_id` and `readiness_timeout_ms`.
Canonical encoding sorts unordered maps/field sets, preserves semantically
ordered step lists, and includes every typed field and protocol major. It is an
explicit versioned, length-delimited encoding, not language `Debug` output or
an iteration-order-dependent map serialization.

Before submission, `MediatorSystemConfigurator` computes a canonical mutation
fingerprint and asks an injected durable `IdempotencyStore` for the key bound to
that fingerprint and the authenticated caller. The store generates
collision-resistant opaque keys and persists the binding before transport I/O.
An ambiguous submission retry, adapter recreation, or client process restart
therefore reuses the same key while that logical mutation is nonterminal.
Terminal bindings are retained for the client retry window (at least 30 days)
and then may be removed; a later explicit submission is a new logical mutation
with a new key. Cleanup never removes a nonterminal binding. If the mediator
returns `idempotency_expired`, the adapter reports that terminal result, retires
its local binding, and invalidates the cached revision/freshness for the
affected resource. A direct retry is rejected as refresh-required; only a later
caller submission after repository refresh and new explicit confirmation
allocates a replacement key.

The idempotency record key is
`(authenticated UID, security label, idempotency_key)`, and its stored value
includes the canonical typed mutation payload digest and generated operation
ID. The caller cannot select an operation ID. An identical retry returns the
original operation/report; a different payload under that key returns
`idempotency_conflict`. Poll and cancel reacquire transport credentials and
require the same UID/security-label owner, otherwise returning `not_found`
without revealing another caller's operation. These checks also apply after
reconnect.

Idempotency and operation records are transactionally persisted in a
root-owned durable store before effects begin and recovered after mediator
restart. Nonterminal records do not expire. Full reports remain available for
at least 30 days measured from entry into terminal state. Compaction may replace
a report with an `idempotency_expired` tombstone, but the caller/key/payload
digest and operation ID are retained permanently; an expired retry never
executes again and directs the UI to refresh and submit a new key after explicit
confirmation. This favors fail-closed behavior over ambiguous duplicate
mutation.

Before each effect, the mediator durably records the typed step as in-flight;
afterward it records the snapd change ID and outcome before advancing. Recovery
reconciles an in-flight step with snapd and read-back state before deciding
whether it is safe to continue. It never blindly replays an effect whose
completion is ambiguous.

States are `pending`, `authorizing`, `running`, `restarting`, `reconciling`,
`succeeded`, `failed`, or `cancelled`. Progress is optional
`{done,total,unit,summary}`; absence means indeterminate. Cancellation is
best-effort and returns the authoritative report. It transitions only a
cancellable nonterminal operation; cancellation after `succeeded`, `failed`,
or `cancelled` preserves and returns that terminal report.

### Mutation execution and reconciliation

`ApplyBackendConfig` maps one-for-one from the current apply preview:
allowlisted package assignments become one typed `SnapSet`; model and engine
selectors become typed `UseModel`/`UseEngine` steps; and the exact explicit
`snap restart <backend>` becomes a typed `Restart` step. After one
caller-bound `configure-backend` authorization, the mediator executes those
effects sequentially in that order, performs restart when requested, waits up
to `readiness_timeout_ms`, and always performs a final backend read-back.

`SwitchBackend` maps the current plan's ordered disconnects, one connect, and a
final restart of the host `myna.myna` user daemon so it sees the new backend
content mount. Dictation may pause briefly while that daemon becomes ready
again. It uses one `switch-backend` authorization, executes sequentially, then
always rediscovers connections. This improves the host path only and does
**not** claim to fix the separate strict-shipping path. Neither operation
claims rollback or atomicity across snapd calls. Each report includes an
ordered structured step outcome (`kind`, redacted typed target, state, snapd
change ID if any, stable error), the final read-back snapshot and revision, and
whether requested state matches observed state. A failed step stops later
effects; partial effects and the reconciled final snapshot remain visible.
Readiness timeout is `readiness_timeout`, not success. A revision mismatch
before the first effect is `conflict` and performs no mutation. Revision
changes during execution are reported with the final snapshot so the UI
refreshes rather than overwriting newer state.

### Errors and privacy

Stable error codes are:

`invalid_request`, `unauthenticated`, `authorization_denied`,
`authorization_cancelled`, `not_found`, `conflict`, `unsupported`,
`incompatible_protocol`, `mediator_unavailable`, `snapd_unavailable`,
`backend_unavailable`, `configure_hook_rejected`, `operation_failed`,
`cancel_not_supported`, `negotiation_required`, `idempotency_conflict`,
`idempotency_expired`, `readiness_timeout`, and `internal`.

Responses contain code, safe user message, retryability, operation ID, and
optional structured field errors. Raw stderr, environment, home paths,
macaroons, polkit details, model prompts/audio, complete snapd responses, and
backend secrets never cross into UI diagnostics. Logs use operation IDs and
allowlisted snap/key names; diagnostics export redacts values by default.

### Packaging and updates

The mediator must be installed and updated by the Ubuntu archive/image or
another platform-owned mechanism, with a root-owned executable/config and a
stable service identity. The strict snap declares only the reviewed client
transport interface. Snap and mediator update independently, so both retain
the previous protocol major during rollout and fail closed on incompatibility.
Absence or mismatch leaves local Myna GSettings functional and renders backend
pages unavailable with installation/update guidance.

## Adapter and command dispatch

The current `ClientSettings`, `BackendRepository`, and `SystemConfigurator`
ports remain the domain/UI boundary:

- `GioClientSettings` continues to access only Myna's private keyfile.
- A future `MediatorBackendRepository` maps discovery/read calls and typed
  wire DTOs into existing `ConnectionSnapshot`/`BackendSnapshot` values; domain
  snapshots are never transport types.
- A future `MediatorSystemConfigurator` maps apply/switch plans to the typed
  mutation operations (including restart), retries ambiguous submission with
  its durable idempotency key, then polls/subscribes by mediator-generated
  operation ID until a terminal report. Polling honors caller cancellation and
  uses an injected bounded wait policy with a deadline around every poll and
  cancel transport call, in addition to a maximum poll count, so transport
  loss, a never-resolving call, or a stuck operation is an unavailable error,
  never success or an unbounded wait. A best-effort cancellation response that
  remains nonterminal is polled within the same bound and cannot become
  optimistic success. Stable contract errors and ordered step outcomes
  map into
  `SystemConfiguratorFailure`/`CommandResult`; specifically,
  `values_rejected` maps to
  `SystemConfiguratorError::ValuesRejected`.
- No domain model or GTK presenter receives transport, snapd, polkit, or
  protocol types. The existing `SystemConfigurator` port is already the seam:
  a mediator client implements it without any consumer changing. Nothing here
  claims compatibility with unimplemented production IPC.

After the mediator gate is approved and implemented, the installed wrapper
dispatch must be:

- no arguments: launch the native GUI;
- explicit `get`, `set`, `reset`, `reset-recursively`, `range`, `describe`,
  `list-keys`, `list-children`, `list-recursively`, `monitor`, and `writable`:
  execute the existing GSettings wrapper path with exactly the current
  arguments, output, exit status, schema injection, keyfile backend, and bare
  string behavior;
- all other arguments: preserve the current wrapper's exact pass-through to
  `gsettings` (including its errors); do not reinterpret them as GUI options.

Until that gate succeeds, bare `myna.config` continues its current
`list-recursively` behavior. The native host prototype remains available via
`make run-config`.
