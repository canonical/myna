# Backend-Snap Configuration API — Design Proposal

**Date:** 2026-07-12 (revised 2026-07-15 after team review; revised 2026-07-27 after platform meeting)
**Status:** In progress — positions partially settled; open questions tracked in §6
**Authors:** Charles, with Claude

How the desktop (Settings UI, orchestrator, power user) reads and changes the
configuration of the ASR inference snaps — model selection, engine selection,
residency policy, per-inference tuning — and who owns that boundary.

This document records positions and their rationale, names the required
inference-snap-side API changes, and tracks open questions (owner, mediation
shape, provisioning path). It builds on the snap design note
(`asr-inference-snap-design.md`), capabilities discovery (`myna.core.capabilities`,
T24), and action items from the Configuration-API discussions (2026-07-14/15,
2026-07-27).

## 1. Context

Today **all** backend-snap configuration is reachable only through `modelctl`
(the IE108 CLI vendored from `inference-snaps-cli`):

- `modelctl set <key>=<value>` — **root-only** (`IsRootUser`) and prompts a
  **snap restart** to apply. Three scopes: `--package` (hidden; `socket.path`,
  `verbose`, `sleep-idle-seconds`, seeded by the install hook), `--engine`
  (hidden; the engine manifest's `configurations:` — e.g. `att-context-size`),
  and user config (validated against known keys).
- `modelctl use-model <id>` / `use-engine [--auto]` — root; changes which model
  the socket serves / which engine runs; pulls the needed components; restarts.
- `modelctl get [<key>]` — unprivileged read of current values.

The only surfaces above modelctl are:

- **`capabilities.query`** (T24) — a *runtime, read-only, session-scoped* view
  over the transcription socket: the **active** model, input languages,
  `input_formats`, punctuation, translation. It describes what the running
  server *can do right now*, not what is *configurable*.
- The session envelope (IE115 `session.update` / `SessionConfig`) — per-request
  parameters (language, prompt, timestamps). Unprivileged, per-connection.

There is **no** machine-readable schema of *what is configurable*, **no**
unprivileged path for a UI to *change* anything, and **no** UI-facing story for
downloading new model components. The meeting flagged all three, plus the
ownership question.

## 2. Three layers — do not conflate

The single word "configuration" hides three concerns with different owners,
privilege levels, latencies, and UX:

| Layer | Concern | Mechanism today | Privilege | Latency |
|---|---|---|---|---|
| **1 — Provisioning** | Which snaps installed; which model/runtime **components** downloaded | `snap install`, snap components (snapd) | root / snapd | network-bound; needs progress UX |
| **2 — Configuration** | Per-installed-snap: active engine, active model, residency/idle policy, tuning knobs, socket path | `modelctl set` / `use-model` / `use-engine` | root + restart | seconds (restart) |
| **3 — Session parameters** | Per-utterance: language, prompt, timestamp granularity, per-request model **request** | IE115 `session.update` / `SessionConfig` on the wire | unprivileged, per-connection | live |

**Layer 3 already has a home** (the wire API) and must stay there — it is not
"configuration" in the sense this spec means. The transcription socket is
unprivileged and per-session; it **must not** become a privileged control plane
(that would be a confinement / privilege-escalation surface). This spec is about
**layers 1 and 2**, and about drawing the boundaries so the three do not leak
into each other.

> **⚠ Open (§6):** Whether **language** belongs permanently in layer 3 (per-session
> wire param) or should acquire a layer-2 default (persisted config) is under
> review by platform engineering. Do not hardcode either position until guidance
> arrives.

### Model selection has two distinct verbs (the classic conflation)

- **Session request** (layer 3): a client asks for a model in `session.update`;
  the server **rejects** it (`model_not_available`) if it is not the one this
  process serves (T48, already implemented). This does **not** change anything —
  it is a compatibility probe.
- **Config selection** (layer 2): `use-model` changes *which model the socket
  serves*, pulling the component if absent (layer 1) and restarting. A Settings
  "choose model" control is **this**, not the session request.

A UI must never try to "switch model" by putting a different `model` in
`session.update` — that only ever produces a rejection. The config API owns the
real switch.

## 3. Proposed direction

### 3.1 Config stays out-of-band from the transcription socket

A **separate** config surface, never the session socket. Rationale: the session
socket is unprivileged and reachable by any confined client that can dictate;
folding privileged config into it would make every dictation client a potential
config-mutation vector and entangle it with T17 access control. Read-only
*runtime* discovery (`capabilities.query`) stays on the socket; *settings* do
not.

**Transport: UDS via content share — TCP is ruled out.** The `myna` client snap
is strictly confined with no `network` plug, so a TCP-only backend is
unreachable by design. The correct topology — UDS socket exposed through a
`ubustt-socket` writable content share — is already how the whisper-snap is
packaged and must remain the norm for any community inference snap. Any
proposed config surface (§3.3) must sit on the same UDS/content-share transport,
not on TCP.

### 3.2 CLI first; specify the shape a UI will later consume

Per the meeting ("Configuration UI · Start with a CLI"), v1 is literally
`modelctl` (aliased per-snap, e.g. `whisper set …`, `whisper use-model base`).
But we specify the *shape* of the eventual programmatic surface now so the CLI
and a later programmatic API converge instead of diverging. The CLI is the first
consumer of the same schema (§3.4) a GUI will later consume.

### 3.3 Mediating privileged writes: snapd-managed config + a configure hook

**What "mediation" means here.** An unprivileged Settings panel cannot run
`sudo modelctl` — it lacks the privilege and would break confinement. Something
must sit between the UI and the privileged snap operations (root + restart +
component pulls) and authorize the user before acting. That intermediary is the
*mediator*. Options are: a bespoke root daemon, a D-Bus service, or
**snapd itself** via its config API + configure hooks. The chosen path uses
snapd as the mediator, reusing infrastructure that already exists.

An unprivileged Settings panel cannot run `sudo modelctl`. Something must mediate
root + restart + component pulls behind an authorization prompt. This is the
meeting's "external root daemon that can pull new components".

**Direction change (team review, 2026-07-14/15).** Two platform constraints
reshape this section *away* from the earlier D-Bus-daemon recommendation:

- **No new D-Bus API** (advice from Jon S.). The team originally wanted
  inference-snap discovery (mDNS-over-D-Bus) *and* transcription-over-D-Bus, and
  dropped both. Since transcription now rides **UDS**, configuration should
  use the **same transport layer** — one uniform socket family, not a second D-Bus
  control surface.
- **modelctl's storage backend is moving to snapd-managed config.** modelctl
  today uses snapd as its storage backend, but inference snaps aren't allowed
  direct access to that (unstable) storage layer. The plan is to expose the
  **snapd-managed configuration** (`snap set` / `snap get`) so config can be
  **seeded from a Gadget snap on Ubuntu Core**. That exposes *just the
  configuration*; a snap **`configure` hook** can then do the heavy lifting —
  install components and restart — the way modelctl does today.

So the mediation story becomes two lanes over reused infrastructure, **not** a
new privileged daemon:

- **Reads and rich clients** — a config surface over **UDS** (see §3.1 — TCP is
  ruled out), *separate* from the transcription session socket but on the same
  transport family. **Protocol: gRPC preferred**, but D-Bus remains on the table
  if it proves a more appropriate fit for the specific integration (e.g. tighter
  desktop policy, existing tooling). The protocol choice is a soft preference,
  not a closed decision. gRPC arguments: protobuf schema doubles as
  machine-readable contract, native server-streaming suits provisioning progress
  (§3.8 / Appendix A.3), multi-language client support. D-Bus arguments:
  already the system integration bus on Ubuntu Desktop, well-understood policy,
  avoids a second socket. Settle this with the platform team before building.
  (This is the config/control plane only; the transcription wire stays
  WebSocket-over-UDS / IE115 as today.)
- **Privileged writes** — go through **snapd-managed config**: the frontend sets
  keys via the **snapd REST API** (equivalent of `snap set <snap> key=value`),
  which the desktop already reaches and which **snapd itself gates** with its own
  polkit authorization. A **`configure` hook** in the snap reacts — validates,
  pulls any needed components (layer 1), and **automatically restarts** (or
  defers; see restart policy below). The UI must surface a "restarting…"
  indicator while the socket is down and signal readiness when it comes back.
  Silent restarts are not acceptable — the user must see feedback.

This **reuses existing infrastructure** (the earlier option (c)) rather than
standing up a privileged D-Bus daemon with its own polkit policy: no new
always-on component to own and confine, no snap-level `polkit` declaration to
negotiate with the store, and authorization handled by snapd where the user
already grants install/config prompts. The AppArmor/polkit self-authorization
findings from the Marco review (a strict snap *can* ship polkit config and own a
D-Bus name) remain **true but are no longer the chosen path** — kept in
Appendix B as fallback evidence should a snap-local daemon ever be needed.

**Restart policy (settled 2026-07-27).** When a configuration change requires a
restart, the configure hook **triggers the restart automatically** — no manual
intervention required. The UI must show the user a "restarting backend…" state
while the socket is down and confirm readiness when the new process is serving.
`--no-restart` remains available for batching multiple writes (apply once at
the end) and for Gadget-seeding / provision-time config, but the default for
a user-initiated Settings change is automatic restart with feedback.

`modelctl set` / `unset` / `use-model` /
`use-engine` accept a **`--no-restart`** flag: the change is recorded but not
applied until a separately-triggered restart. This lets a UI batch several
changes (model + engine + idle policy) and restart **once**, lets the
configure-hook / Gadget-seeding path record config at provision time and apply it
on next start, and makes the restart an explicit, observable step rather than a
side effect of every `set`. The schema's `restart_required` (§3.4) tells the UI
*whether* a change leaves a restart pending; the restart itself is a separate
verb.

**Superseded options (recorded for the team).** The earlier draft weighed three
broker shapes; the review resolves the choice:

- ~~(a) polkit-wrapped `modelctl` invocations~~ — still a possible MVP shim, but
  `pkexec`-ing a confined snap binary is awkward.
- ~~(b) a small root D-Bus daemon~~ — **ruled out** by the no-new-D-Bus guidance.
- **(c) snapd's own configuration** (`snap set` / snapd REST) mediated — **now the
  direction**, paired with a `configure` hook for the install-and-restart work
  and a UDS read surface for rich clients.

This is **not STT-specific** — it belongs to the inference-snaps platform, which
argues for device-engineering ownership (§4).

### 3.4 Config-schema discovery — the key required API change

For a UI to render controls without hardcoding per-snap knowledge, each snap must
**advertise its configurable keys** in machine-readable form: for each key its
**type**, **allowed values / range** (enum for `model` ∈ options and engine ∈
detected-compatible; int≥0 for `sleep-idle-seconds`; enum/free for
`att-context-size`), **default**, **scope** (package/engine/user), and whether
changing it **requires a restart**.

**Prefer a standardized config vocabulary over per-snap dynamic schemas (team
guidance from the top, 2026-07-15).** The direction from leadership is to
**enforce a uniform set of configurations** across backends rather than let every
snap expose its own dynamic key set and schema — otherwise the user faces a
different, unfamiliar pile of knobs per backend. Fully uniform isn't 100%
feasible (some models/runtimes genuinely need their own tweaks, e.g.
`att-context-size`, `compute-type`), so the working split is:

- **A standard core vocabulary** — the same keys, names, types, and semantics on
  *every* inference snap: `model`, `engine`, `sleep-idle-seconds`, `verbose`,
  `socket.path`. A UI renders these once and they mean the same thing everywhere.
- **A small, clearly-marked backend-specific tail** — the genuinely
  model/runtime-specific knobs, in a bounded set, flagged so a UI can group them
  under "advanced" and not treat them as first-class.

Schema *discovery* therefore covers only what is **necessarily dynamic** — the
values, domains, and per-machine availability — not an open-ended, per-snap
key universe. Two of those dynamic pieces already have homes in modelctl:

- the **model list** comes from **`modelctl list-models`** (not a bespoke
  discovery call) — the schema's `model` enum is derived from it;
- **`restart_required` is deterministic**, known per key, not something a snap
  reports dynamically per write — the schema states it as a fixed property.

modelctl today has the *scopes* and *values* but **no schema of the domains** —
nothing says "`sleep-idle-seconds` is a non-negative integer" or "`model` is one
of {tiny, base, small}". This is the central inference-snap-side change:

- a **single** `modelctl describe-config` (or `config-schema` / `--format=json`
  on an existing command) that emits the *whole* key set in one call —
  types/domains/defaults/scope/restart-required — **not** a per-setting call
  (Marco review: minimise UI process spawns). Each key also carries its
  `current` value (Appendix A), so this one read doubles as get-all: the UI
  populates the entire panel from it with no follow-up `modelctl get` per key,
  and
- for enums whose domain is hardware-dependent (engine, model options gated by
  installed components), the schema must reflect *what is actually installable/
  selectable on this machine*, not just the manifest superset.

This is the **write-side sibling of capabilities (T24)**: capabilities describes
the running model's *runtime* abilities; the config schema describes the *knobs*
and their domains. They should be consistent (e.g. the model enum in the schema
⊇ the active model capabilities lists) but serve different consumers.
A concrete strawman of this schema (whisper-snap) is in **Appendix A**.

### 3.4.1 Gap: model capabilities query API (not yet in modelctl)

**⚠ New gap identified 2026-07-27.** The current `modelctl` exposes snap-level
configuration (socket path, idle policy, engine, active model) but has **no API
to query the capabilities of the model itself** — e.g. which languages it
supports, what model variants it offers, what accuracy/speed trade-offs each
variant implies. This is distinct from both:

- `capabilities.query` on the transcription socket (T24) — runtime state of the
  *currently running* model, not a static property of the model family; and
- `describe-config` (§3.4) — knobs that can be changed, not what each option
  *can do*.

What is missing is something like `modelctl describe-model <id>` (or a
`models` section inside `describe-config`), returning per-model:

```json
{
  "id": "base",
  "title": "Whisper base (74M)",
  "languages": ["en", "fr", "de", "es", "..."],
  "multilingual": true,
  "variants": [
    { "engine": "cpu",        "compute_type": "int8",       "notes": "fastest on CPU" },
    { "engine": "nvidia-gpu", "compute_type": "float16",    "notes": "full accuracy" }
  ],
  "disk_size": "150M",
  "installed": true
}
```

This is needed for:
- A Settings "choose model" control that tells the user what each option *does*
  (language coverage, quality tier), not just its name.
- Model profile mapping (§3.7) — a profile recommendation requires knowing each
  model's characteristics.
- UD136's "Download language models?" UX — showing what you get before you
  download it.

**Owner: TBD** (same ownership question as describe-config, §4). This should be
part of the platform team's `describe-config` deliverable, or a companion
`describe-model` command. The strawman in Appendix A already embeds partial
per-option metadata (size, capabilities tags) — that section should be extended
to cover the full model capability surface once ownership is confirmed.

### 3.5 Unprivileged read, snapd-mediated write

Displaying current settings must not require root. `modelctl get` is already
unprivileged; the schema (§3.4) read must be too. So: a UI **reads** config +
schema directly (unprivileged), and **writes** through snapd-managed config
(§3.3) — which snapd gates with its own authorization prompt. This keeps the
common case (show me my settings) friction-free and puts the auth prompt only
where a mutation actually happens.

The read is a **single get-all at UI startup**: one `describe-config` returns the
schema *and* every key's `current` value (§3.4, Appendix A), so a panel renders
and populates from one call — no per-key reads. Served over the UDS read
surface (§3.3), the UI reads it without shelling out per key.

### 3.6 Engine selection: device-attribute matching auto by default, manual override as a knob

**What was removed is the memory/VRAM *capacity* gate, not device matching**
(Farshid, 2026-07-08 + follow-up). `use-engine --auto` still scores engines on
rich **device-attribute matching** — it is *not* a coarse present/absent check:

- **GPUs:** device vendor, device *model* (e.g. Jetson Orin usually needs a
  separate build), **NVIDIA compute capability**, **AMD microarchitecture**.
- **CPUs:** AMD64 CPU flags / ARM64 CPU features (to target the expected
  instruction sets), and in some cases the manufacturer-id (CPUID) — e.g. to
  match an OpenVINO-based engine to all Intel CPUs, since the runtime itself
  detects and uses the supported instructions.

What is gone is the **VRAM/memory quantity gate**: total/available VRAM at
install time is stale by startup (other apps take chunks), split-load and
unified-memory platforms make a single number wrong, and NVIDIA unified memory
does not report VRAM at all. So an engine is selected on *whether the hardware is
the right kind*, never on *whether a model will fit*. Consequences for this API:

- **Default is auto** (install hook already runs `use-engine --auto`); 95% of
  users never touch engine selection.
- Expose a **manual override** (force `cpu` vs `nvidia-gpu`) as a config knob for
  the power user / debugging, defaulting to auto. This override **already exists**
  — `modelctl use-engine <engine>` — so the API surfaces it rather than inventing
  it. The schema's engine enum is the set of engines *compatible with detected
  hardware*, and each option should carry **why** it matched or was ruled out
  (the attribute that decided it — compute capability, microarch, CPU flag,
  vendor). That "why" is **already available from `modelctl show-engine
  <engine>`**, so the schema's per-option `matched_on`/`reason` (Appendix A.2) is
  *derived from* `show-engine`, not a new data source.
- **No capacity pre-gating anywhere.** A model that will not fit is *attempted*
  and fails **observably on the wire** (the `preparing` → terminal-error
  lifecycle; codes are T31's), never silently. T12 sizing stays as
  *guidance/defaults*, not gates.

### 3.7 Residency / idle policy is layer-2 config, exposed as intent

`sleep-idle-seconds` + idle-action (T27/T29) are config keys. **What `sleep-idle-
seconds` measures (clarified 2026-07-15):** it is the number of seconds between
when the API **finishes responding** to a request and when the server **unloads
the model** — i.e. it is the *definition of idle* (idle = time since the last
response), not a poll interval or a hard cap. The CLI keeps the
raw knob; a future Settings control should expose **intent** ("keep dictation
instantly ready" vs "free memory when idle"), never the raw seconds or the
unload mechanism (per T29/T30). The residency **default** (T29) is the product
for most users; this API is how the 5% deviate. **Coupling (T29):** the idle
default and the client capture ring-buffer depth are one decision — the buffer
must cover the worst-case cold load the policy tolerates, or pre-ready speech is
lost.

**Model profiles (UD136: default/lightweight/quality) — position 2026-07-27.**
Computing profiles from first principles (mapping profile → model + engine +
compute-type automatically from hardware + benchmarks) is the ideal but is
not practical in the first iteration. The agreed approach:

- **First iteration: recommend, don't enforce.** After the initial benchmarking
  run (already done — `results/`), surface a *recommended* profile to the user
  based on their hardware. Do not auto-install or auto-switch snaps — installing
  a snap from inside a snap is unsupported and out of scope.
- **Machinery needed first:** model profile recommendations require the model
  capability query API (§3.4.1) so the UI knows what each option provides. The
  profile metadata (accuracy tier, resource tier, recommended-for) should live
  as tags in the config schema's model options (Appendix A) rather than being
  hardcoded per-snap in the UI.
- **No auto-correction.** If a different snap family would serve the user better,
  the UI can surface that as a suggestion pointing at Software Center, not as an
  automated action.

### 3.8 Component provisioning + progress (layer 1)

"Download another model" is a **component install** via snapd, driven by the
snapd-managed-config / configure-hook path (§3.3) or by pointing the user at
Software Center / a command (the meeting's "tell the user to go run a command?"
fallback). Choosing *other inference snaps entirely* (a different family) is
**out of scope here** — that is general inference-snap discovery / Software
Center territory, not STT-specific.

**Progress: a snap limitation, not a design choice (clarified 2026-07-27).**
There are two vantage points:

- **The desktop frontend is outside the confinement.** If it already talks to the
  **snapd REST API** for configuration (`snap set`, §3.3), it can use that *same*
  API to observe **component download progress** — snapd exposes change/task
  progress today, from outside the snap, without `snapd-control`. So a Settings
  UI can show real byte-level progress **now**, by watching the snapd change it
  initiated. This is the preferred path and does not wait on snapd 2.77/2.78.
- **modelctl, inside the snap, cannot see it** — a confined snap without
  `snapd-control` has no access to that progress, so `modelctl` itself shows only
  an **indeterminate spinner + "downloading…"** until snapd exposes
  component-download progress to the snap (no committed version as of 2026-07-27).
  That spinner is the CLI's honest MVP, *not* a limit on the GUI.

**Snapd version dependency is a platform constraint, not something we can resolve
unilaterally.** Using snap components gives us model packaging but no in-snap
progress. Better in-snap progress requires a future snapd release. The UD136
Settings UX depends on which snapd features have landed; track those with the
snapd team but do not gate the STT design on a specific version.

## 4. Ownership (open — still unresolved as of 2026-07-27)

Proposed split, still to be confirmed:

- **Device engineering** owns the **config surface + schema + snapd-config
  mediation** (§3.3–3.5) and the provisioning path (§3.8) — these are the
  modelctl/IE108 platform, per-snap and **not** STT-specific; every inference
  snap benefits. This includes `describe-config` and the model capability query
  API (§3.4.1).
- **The STT team (us)** owns the **STT-specific keys** (`att-context-size`, model
  options, residency intent mapping), the model capability metadata surface
  (§3.4.1 — we drive the spec even if device engineering builds it), and the
  **desktop Settings integration** (UD129 scope) that consumes the schema.
- **Software Center integration** for choosing other inference snaps is **out of
  scope** for this spec (not STT-specific).

**⚠ Gap:** The pluggability contract for community / third-party inference snaps
(what a snap must implement to be picked up as a backend — IE115 dialect,
`capabilities.query`, `ubustt-socket` content share, config schema) needs a
**clear public developer document with tutorials and examples**. This is the
T48-adjacent work (backend discovery) and is currently unowned. It is not
pure STT scope nor pure platform scope — ownership must be explicitly assigned.

## 5. Required inference-snap-side API changes (summary)

1. **Config-schema discovery** (§3.4) — machine-readable keys with
   type/domain/default/scope/restart-required, domains reflecting what is
   actually selectable on this machine. *The central change.* Owner TBD (§4).
2. **Model capabilities query API** (§3.4.1) — per-model: supported languages,
   multilingual flag, available variants, disk size, quality/resource tier tags.
   **⚠ New gap** — not currently in modelctl. Needed for Settings model picker,
   model profiles (§3.7), and UD136 download UX.
3. **Snapd-mediated write path** (§3.3) — config as **snapd-managed config**
   (`snap set` via the snapd REST API, gated by snapd's own authorization) plus a
   snap **`configure` hook** that installs components and **automatically restarts
   with user-visible feedback**. No new D-Bus API; protocol for the read/control
   surface (gRPC vs D-Bus) is a soft preference to settle with the platform team.
4. **Unprivileged schema read** (§3.5) — parity with the already-unprivileged
   `modelctl get`.
5. **Component install + progress** (§3.8) — trigger/observe model-component
   downloads from a UI; a frontend with snapd REST access can read progress now;
   in-snap CLI progress is blocked on a future snapd release (platform constraint).
6. **Restart with user feedback** (§3.3) — the configure hook triggers automatic
   restart; the UI must surface a "restarting…" indicator and signal readiness.
   This is **not** a nice-to-have — silent restarts are unacceptable.
7. **Pluggability contract document** (§4) — a public developer doc + tutorials
   so community snap authors know exactly what to implement. Currently unowned.

## 6. Open questions — status after 2026-07-27 meeting

| # | Question | Status | Notes |
|---|---|---|---|
| 1 | **Owner of describe-config** | 🔴 Open | Not resolved. Additionally, current modelctl has no model-capability query at all (§3.4.1 — new gap). |
| 2 | **Mediation shape confirmed?** | 🟡 Partial | Snapd-managed config + configure hook is the direction; platform team still needs to unblock the storage layer. |
| 3 | **gRPC vs D-Bus for control plane** | 🟡 Soft | gRPC preferred; D-Bus acceptable if more appropriate. Settle with platform team before building. |
| 4 | **Pluggability contract for community snaps** | 🔴 Open | Needs a public developer document + tutorials + examples. Currently unowned (T48-adjacent). |
| 5 | **UDS vs TCP transport** | ✅ Settled | **TCP ruled out for security.** UDS via content share (`ubustt-socket`) is mandatory — the client snap has no `network` plug. |
| 6 | **Restart-to-apply for MVP?** | ✅ Settled | Restart **is** automatic and **must** show user feedback (progress / readiness). Silent restarts are not acceptable. |
| 7 | **Model profiles (default/lightweight/quality)** | 🟡 Scoped | First iteration: recommendation after benchmarking, not auto-install. Requires model capability API (§3.4.1). Installing a snap from a snap is out of scope. |
| 8 | **Snapd version dependencies** | 🟡 Noted | Platform constraint, not a design choice. Frontend-via-snapd-REST progress works today; in-snap progress blocked on a future snapd release. Track with snapd team; do not gate STT design on a specific version. |
| 9 | **Language: layer 2 or layer 3?** | 🔴 Open | Waiting on platform engineering guidance. Do not hardcode position. |
| 10 | **T48 sunset / internal dialect** | ➡ Deferred | Leave as-is; other protocols will likely follow. |
| 11 | **Schema authority** | 🔴 Open | Waiting on platform team (same as ownership, item 1). Prefer derived from engine/model manifests (no drift). |
| 12 | **T31 error alignment** | 🟡 Scoped | Design team owns UX: two user-facing error classes — **critical** (service unavailable) and **recoverable** (degraded, partial service). Detailed wire codes (T31) are the *content* delivered within these two types, not additional UX types. Align UD136 error states to these two classes. |
| 13 | **T53 modelctl multi-model sync** | 🟡 Clarified | See §7 for full expansion. |

- **Gadget-snap seeding** — does exposing snapd-managed config change key
  names/scopes we should align to now? Still open; flag to platform team when
  the storage layer unblocks.
- **Provisioning home** — snapd/configure-hook or Software Center? Still open for
  cross-snap/cross-family provisioning. Within-snap component install: configure
  hook (§3.3).

## 7. Relationship to existing work

- **T24 capabilities** — read-side *runtime* discovery; the config schema (§3.4)
  is its write-side sibling; keep them consistent. The model capabilities query
  (§3.4.1) is a static complement — what the model *can* do, independent of
  what is currently running.
- **T48 backend discovery** — the enumeration layer the config API sits on;
  unowned; interacts with the pluggability contract (§4).
- **T29 residency default / T30 dev toggles** — §3.7; the config API is how users
  deviate from the T29 default; T30's power-user "out" is a subset of this.
- **T17 access control** — snapd's authorization on `snap set` is the write-side
  of the socket access-control decision; keep them coherent.
- **T31 error taxonomy** — config-change failures (component pull failed, model
  incompatible) need codes, but the UX layer sees only two classes: **critical**
  (service unavailable — user cannot dictate) and **recoverable** (degraded
  service — dictation works at reduced quality or with a fallback). The T31 wire
  codes are the *machine-readable detail* delivered within those two UX classes
  and are the design team's responsibility to map to copy + actions. UD136's
  error states ("speech model not installed", "inference backend unavailable",
  "selected language unsupported") should be classified as critical vs recoverable
  before those states can be fully specified.
- **T53 modelctl multi-model sync** — this refers to work (pending Farshid's
  changelog) to make modelctl aware of *multiple model slots* — i.e. a backend
  that can serve more than one model variant, or a modelctl that coordinates
  across several installed inference snaps rather than one. The concern for the
  config API: if T53 changes the modelctl topology (e.g. per-slot keys, per-slot
  selectors, a `slots` array in describe-config), building `describe-config` (§3.4)
  against the current single-model shape risks immediate misalignment.
  **Action: sync with Farshid before finalising the describe-config shape** to
  confirm whether multi-model support changes the top-level schema structure
  (e.g. is `active.model` still a single value, or does it become a list/map?).

## Appendix A — concrete `describe-config` schema (whisper-snap example)

A tangible strawman of §3.4 for the team to react to: the JSON a UI would read
to render a settings panel for one installed snap, without any hardcoded
per-snap knowledge. Grounded in whisper-snap's **actual** keys (package:
`socket.path`/`verbose`/`sleep-idle-seconds`; engine `configurations:`
`compute-type` on nvidia-gpu; the `use-model`/`use-engine` selectors). Output of
a proposed `modelctl describe-config --format=json` (unprivileged read, §3.5).

### A.1 Shape

Top level = snap identity + the currently-active selectors + a flat `keys` list.
Each key is self-describing enough to render a control and to validate a write
before submitting it.

Common fields on every key:

- `key` — the modelctl key (or the selector verb's target).
- `title` / `description` — human-facing (i18n is the UI's problem).
- `type` — `enum` | `integer` | `boolean` | `string`.
- `scope` — `package` | `engine` | `user` | `selector` (the `use-model`/
  `use-engine` verbs are modelled as `selector` keys, not plain `set`).
- `default`, `current` — the manifest default and the live value.
- `restart_required` — does applying this drop the socket (§5.5)?
- `privileged` — does the **write** go through snapd-managed config / the
  `configure` hook (§3.3, §3.5)? (Reads never do.)
- type-specific domain: `options` (enum), `min`/`max`/`unit`/`special` (integer).

Hardware/provisioning-dependent enums (`model`, `engine`) carry per-option
availability so the UI can show "installed / downloadable / incompatible"
(§3.4, §3.6, §3.8) rather than a bare string list.

### A.2 Example

```json
{
  "snap": "whisper",
  "schema_version": "1",
  "active": { "engine": "cpu", "model": "base" },
  "keys": [
    {
      "key": "model",
      "title": "Model",
      "description": "Which Whisper model this backend serves.",
      "type": "enum",
      "scope": "selector",
      "verb": "use-model",
      "default": "tiny",
      "current": "base",
      "restart_required": true,
      "privileged": true,
      "options": [
        { "value": "tiny",  "title": "Whisper tiny (39M)",  "installed": true,  "installable": true, "disk_size": "80M",  "capabilities": ["multilingual"] },
        { "value": "base",  "title": "Whisper base (74M)",  "installed": true,  "installable": true, "disk_size": "150M", "capabilities": ["multilingual"] },
        { "value": "small", "title": "Whisper small (244M)", "installed": false, "installable": true, "disk_size": "500M", "capabilities": ["multilingual"] }
      ]
    },
    {
      "key": "engine",
      "title": "Compute engine",
      "description": "Auto-selected from detected hardware; override for debugging.",
      "type": "enum",
      "scope": "selector",
      "verb": "use-engine",
      "default": "auto",
      "current": "cpu",
      "restart_required": true,
      "privileged": true,
      "options": [
        { "value": "auto",       "title": "Automatic",  "compatible": true },
        { "value": "cpu",        "title": "CPU",        "compatible": true,  "matched_on": ["arch:amd64", "flag:avx2"] },
        { "value": "nvidia-gpu", "title": "NVIDIA GPU", "compatible": false, "reason": "no GPU device from vendor NVIDIA (0x10de)", "requires": { "gpu_vendor": "nvidia", "compute_capability": ">=7.0" } }
      ]
    },
    {
      "key": "sleep-idle-seconds",
      "title": "Unload model when idle",
      "description": "Release the model after this many idle seconds (0 = never).",
      "type": "integer",
      "scope": "package",
      "default": 300,
      "current": 300,
      "min": 0,
      "unit": "seconds",
      "special": { "0": "never unload" },
      "restart_required": false,
      "privileged": true,
      "intent": {
        "maps_to": "residency",
        "presets": [
          { "id": "instant",     "title": "Keep dictation instantly ready", "value": 0 },
          { "id": "balanced",    "title": "Balanced",                        "value": 300 },
          { "id": "save-memory", "title": "Free memory when idle",           "value": 30 }
        ]
      }
    },
    {
      "key": "compute-type",
      "title": "Compute precision",
      "description": "CTranslate2 precision (nvidia-gpu engine only).",
      "type": "enum",
      "scope": "engine",
      "default": "float16",
      "current": "float16",
      "restart_required": true,
      "privileged": true,
      "available": false,
      "unavailable_reason": "active engine is 'cpu'; key belongs to 'nvidia-gpu'",
      "options": [
        { "value": "float16" },
        { "value": "int8_float16" },
        { "value": "int8" }
      ]
    },
    {
      "key": "verbose",
      "title": "Verbose logging",
      "type": "boolean",
      "scope": "package",
      "default": false,
      "current": false,
      "restart_required": true,
      "privileged": true
    },
    {
      "key": "socket.path",
      "title": "Session socket path",
      "type": "string",
      "scope": "package",
      "default": "$SNAP_COMMON/run/ubustt.sock",
      "current": "/var/snap/whisper/common/run/ubustt.sock",
      "restart_required": true,
      "privileged": true,
      "advanced": true
    }
  ]
}
```

Notes on the strawman:

- **`engine` gets a synthetic `auto` option** the manifest doesn't have — it maps
  to `use-engine --auto` and is the default, per §3.6. `compatible`/`matched_on`/
  `reason`/`requires` come from the same device-**attribute** scoring the install
  hook runs — vendor, device model, NVIDIA compute capability, AMD microarch, CPU
  flags/CPUID — *not* a VRAM/capacity gate. `matched_on`/`requires`/`reason` are
  **sourced from `modelctl show-engine <engine>`** (§3.6), which already explains
  why an engine was chosen or ruled out (e.g. "needs compute capability ≥7.0").
- **`compute-type` is scoped to an engine and reported `available:false`** when a
  different engine is active, so a UI greys it out with a reason rather than
  offering a key that has no effect. nemotron's `att-context-size` is the same
  pattern (engine-scoped enum/free-string).
- **`intent`** on `sleep-idle-seconds` is the optional UI-hint from §3.7: the
  panel shows three presets (intent), the raw integer stays for the CLI/power
  user. `restart_required:false` because idle-unload is in-process (T27).
- **Availability is per-machine**, not the manifest superset (§3.4): `small` is an
  option but `installed:false` — selecting it triggers a component pull (§A.3).
- The schema is **derived** from the engine/model/package manifests + live
  modelctl state (§6, "schema authority"), so it never drifts from what the snap
  actually accepts.

### A.3 Provisioning + progress (layer 1) — event shape

When a write selects a not-yet-`installed` option (or the user adds a model), the
configure-hook / snapd-config path (§3.3) drives the snapd component install. A
frontend with snapd REST access watches the resulting snapd **change** for
progress directly (§3.8); the shape below is the *normalized* view a
UDS read surface (or the UI's own snapd-change adapter) would present:

```json
{ "type": "provision.progress", "snap": "whisper", "component": "model-small", "phase": "downloading", "done_bytes": 261881856, "total_bytes": 524288000 }
{ "type": "provision.progress", "snap": "whisper", "component": "model-small", "phase": "installing" }
{ "type": "provision.done",     "snap": "whisper", "component": "model-small" }
{ "type": "provision.error",    "snap": "whisper", "component": "model-small", "code": "download_failed", "message": "network unreachable" }
```

A frontend reading snapd's change/task progress gets byte-level
`downloading`/`installing`/done **today**; only the *in-snap* `modelctl` view is
limited to `phase` transitions with an indeterminate spinner until snapd 2.77
(§3.8). Error `code`s align with T31, not ad-hoc strings.

## Appendix B — fallback: snap-local polkit / D-Bus (not the chosen path)

Recorded so the option isn't re-litigated. The Marco review (2026-07-14,
verified against snapd source) established that a snap-local privileged daemon
*is* technically possible under strict confinement, should the snapd-managed-
config path (§3.3) ever prove insufficient:

- A **strict-confined** snap can ship polkit configuration via snapd's **`polkit`
  interface** (`interfaces/builtin/polkit.go`, since snapd 2.55): `action-prefix`
  + `meta/polkit/<plug>.*.policy` → `/usr/share/polkit-1/actions`, and
  `install-rules` (sha3-384-pinned) → `/etc/polkit-1/rules.d`. With
  `action-prefix` set the snap is AppArmor-allowed to call polkitd
  `CheckAuthorization`/`RegisterAuthenticationAgentWithOptions`, so a daemon
  *inside the snap* can self-authorize. A classic deb is not required. This would
  need a store-granted `polkit` snap declaration (base decl is
  `allow-installation: false` + `deny-auto-connection: true`).
- snapd supports **D-Bus-activatable** services (since ~2.49): a `daemon` app
  with `activates-on: [<slot>]` on a `dbus` slot starts on first call (no
  always-running daemon); an unconfined client reaches it via snapd's generated
  bus policy with no snap-declaration.

This path is **superseded by the no-new-D-Bus guidance (Jon S., §3.3)** and kept
here only as contingency evidence.
</content>
</invoke>
