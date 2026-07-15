# Backend-Snap Configuration API — Design Proposal

**Date:** 2026-07-12 (revised 2026-07-15 after team review)
**Status:** Proposed — team discussion draft (feeds workstream E; ownership TBD)
**Authors:** Charles, with Claude

How the desktop (Settings UI, orchestrator, power user) reads and changes the
configuration of the ASR inference snaps — model selection, engine selection,
residency policy, per-inference tuning — and who owns that boundary.

This is a **proposal to bring to the team**, not a settled contract. It records
positions and their rationale, names the required inference-snap-side API
changes, and lists the open questions (owner, mediation shape, provisioning path).
It builds on the snap design note (`asr-inference-snap-design.md`), capabilities
discovery (`myna.core.capabilities`, T24), and the meeting action items from the
Configuration-API discussion.

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

### 3.2 CLI first; specify the shape a UI will later consume

Per the meeting ("Configuration UI · Start with a CLI"), v1 is literally
`modelctl` (aliased per-snap, e.g. `whisper set …`, `whisper use-model base`).
But we specify the *shape* of the eventual programmatic surface now so the CLI
and a later programmatic API converge instead of diverging. The CLI is the first
consumer of the same schema (§3.4) a GUI will later consume.

### 3.3 Mediating privileged writes: snapd-managed config + a configure hook (not D-Bus)

An unprivileged Settings panel cannot run `sudo modelctl`. Something must mediate
root + restart + component pulls behind an authorization prompt. This is the
meeting's "external root daemon that can pull new components".

**Direction change (team review, 2026-07-14/15).** Two platform constraints
reshape this section *away* from the earlier D-Bus-daemon recommendation:

- **No new D-Bus API** (advice from Jon S.). The team originally wanted
  inference-snap discovery (mDNS-over-D-Bus) *and* transcription-over-D-Bus, and
  dropped both. Since transcription now rides **TCP/UDS**, configuration should
  use the **same protocol layer** — one uniform transport, not a second D-Bus
  control surface. This also serves clients a system-bus D-Bus API would not:
  future inference-snap clients from a **web app or another machine**.
- **modelctl's storage backend is moving to snapd-managed config.** modelctl
  today uses snapd as its storage backend, but inference snaps aren't allowed
  direct access to that (unstable) storage layer. The plan is to expose the
  **snapd-managed configuration** (`snap set` / `snap get`) so config can be
  **seeded from a Gadget snap on Ubuntu Core**. That exposes *just the
  configuration*; a snap **`configure` hook** can then do the heavy lifting —
  install components and restart — the way modelctl does today.

So the mediation story becomes two lanes over reused infrastructure, **not** a
new privileged daemon:

- **Reads and rich clients** — a config surface over **TCP/UDS**, same protocol
  layer as the transcription socket (§3.1 still holds: a *separate* surface from
  the session socket, same transport family). This is what a web-app / remote /
  cross-machine client would use, and gives us a uniform protocol layer across
  transcription and configuration. **Protocol: gRPC** — with D-Bus off the table,
  JB recommends **gRPC** for this surface. It fits the requirements well: runs
  over both UDS (local, unprivileged) and TCP (remote/cross-machine), has a
  first-class schema (protobuf/proto3) that doubles as the machine-readable
  contract, native streaming (server-streaming RPC suits the provisioning
  progress events of §3.8 / Appendix A.3), and mature multi-language clients for
  future web/desktop consumers. (This is the config/control plane only; the
  transcription wire stays WebSocket-over-UDS / IE115 as today — "same transport
  family", not necessarily the same framing.)
- **Privileged writes** — go through **snapd-managed config**: the frontend sets
  keys via the **snapd REST API** (equivalent of `snap set <snap> key=value`),
  which the desktop already reaches and which **snapd itself gates** with its own
  polkit authorization. A **`configure` hook** in the snap reacts — validates,
  pulls any needed components (layer 1), and restarts (or defers; see
  `--no-restart` below).

This **reuses existing infrastructure** (the earlier option (c)) rather than
standing up a privileged D-Bus daemon with its own polkit policy: no new
always-on component to own and confine, no snap-level `polkit` declaration to
negotiate with the store, and authorization handled by snapd where the user
already grants install/config prompts. The AppArmor/polkit self-authorization
findings from the Marco review (a strict snap *can* ship polkit config and own a
D-Bus name) remain **true but are no longer the chosen path** — kept in
Appendix B as fallback evidence should a snap-local daemon ever be needed.

**Restart is decoupled from the write.** `modelctl set` / `unset` / `use-model` /
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
  and a TCP/UDS read surface for rich clients.

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

### 3.5 Unprivileged read, snapd-mediated write

Displaying current settings must not require root. `modelctl get` is already
unprivileged; the schema (§3.4) read must be too. So: a UI **reads** config +
schema directly (unprivileged), and **writes** through snapd-managed config
(§3.3) — which snapd gates with its own authorization prompt. This keeps the
common case (show me my settings) friction-free and puts the auth prompt only
where a mutation actually happens.

The read is a **single get-all at UI startup**: one `describe-config` returns the
schema *and* every key's `current` value (§3.4, Appendix A), so a panel renders
and populates from one call — no per-key reads. Served over the TCP/UDS read
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

### 3.8 Component provisioning + progress (layer 1)

"Download another model" is a **component install** via snapd, driven by the
snapd-managed-config / configure-hook path (§3.3) or by pointing the user at
Software Center / a command (the meeting's "tell the user to go run a command?"
fallback). Choosing *other inference snaps entirely* (a different family) is
**out of scope here** — that is general inference-snap discovery / Software
Center territory, not STT-specific.

**Progress: read it from snapd, not from inside the snap (clarified
2026-07-15).** There are two vantage points, and they differ:

- **The desktop frontend is outside the confinement.** If it already talks to the
  **snapd REST API** for configuration (`snap set`, §3.3), it can use that *same*
  API to observe **component download progress** — snapd exposes change/task
  progress today, from outside the snap, without `snapd-control`. So a Settings
  UI can show real byte-level progress **now**, by watching the snapd change it
  initiated. This is the preferred path and does not wait on snapd 2.77.
- **modelctl, inside the snap, cannot see it** — a confined snap without
  `snapd-control` has no access to that progress, so `modelctl` itself shows only
  an **indeterminate spinner + "downloading…"** until **snapd 2.77** exposes
  component-download progress to the snap. That spinner is the CLI's honest MVP,
  *not* a limit on the GUI.

So the guidance flips from the earlier draft: the byte-percentage story is a
**frontend-via-snapd-REST** capability available now; only the in-snap CLI is
gated on 2.77.

## 4. Ownership (open — the meeting's first question)

Proposed split, to confirm with the team:

- **Device engineering** owns the **config surface + schema + snapd-config
  mediation** (§3.3–3.5) and the provisioning path (§3.8) — these are the
  modelctl/IE108 platform, per-snap and **not** STT-specific; every inference
  snap benefits.
- **The STT team (us)** owns the **STT-specific keys** (`att-context-size`, model
  options, residency intent mapping) and the **desktop Settings integration**
  (UD129 scope) that consumes the schema.
- **Software Center integration** for choosing other inference snaps is **out of
  scope** for this spec (not STT-specific).

## 5. Required inference-snap-side API changes (summary)

1. **Config-schema discovery** (§3.4) — machine-readable keys with
   type/domain/default/scope/restart-required, domains reflecting what is
   actually selectable on this machine. *The central change.*
2. **Snapd-mediated write path** (§3.3) — config as **snapd-managed config**
   (`snap set` via the snapd REST API, gated by snapd's own authorization) plus a
   snap **`configure` hook** that installs components and restarts, so an
   unprivileged UI can request changes without running as root or shelling `sudo
   modelctl`. **No new D-Bus API** (Jon S.); rich/remote reads ride TCP/UDS via
   **gRPC** (JB), the same transport family as transcription.
3. **Unprivileged schema read** (§3.5) — parity with the already-unprivileged
   `modelctl get`.
4. **Component install + progress** (§3.8) — trigger/observe model-component
   downloads from a UI; a frontend with snapd REST access can read progress now,
   in-snap CLI progress gated on snapd 2.77.
5. **(Nice-to-have) apply without full restart** — for language/residency tuning,
   a graceful reconfigure beats a socket-dropping restart; flag as future, not
   MVP. (`--no-restart` already lets writes defer/batch the restart, §3.3.)

## 6. Open questions for the team

- **Owner** of the config surface + schema + snapd-config mediation — device
  engineering (§4)?
- **Mediation shape confirmed?** — the review settles on **snapd-managed config
  + `configure` hook** for writes and **TCP/UDS** for rich reads, *not* a D-Bus
  daemon (Jon S.), with **gRPC** as the proposed protocol for that read/control
  surface (JB). Confirm: (i) the platform team will expose snapd-managed config
  for the inference snaps (currently blocked — no direct access to the unstable
  storage layer, §3.3) and own the `configure`-hook install-and-restart logic;
  and (ii) gRPC is the agreed control-plane protocol (vs plain JSON-over-UDS or
  reusing the WS framing).
- **Gadget-snap seeding** — does exposing snapd-managed config for Ubuntu Core
  Gadget seeding change any key names/scopes we should align to now?
- **Provisioning home** — snapd/configure-hook, or Software Center / snapd
  directly (§3.8)?
- **Restart-to-apply** acceptable for the MVP, or is graceful reconfigure needed
  for the residency/language knobs (§5.5)?
- **Relationship to T48** — the config API needs to know *which snaps/sockets
  exist and what each serves*; backend discovery (T48) is the read-side of that
  and is currently unowned. Decide whether discovery + config schema are one
  surface or two.
- **schema authority** — does the schema live in modelctl (derived from engine/
  model manifests + `configurations:`) or in a new per-snap manifest? (Prefer
  derived — single source of truth, no drift.)

## 7. Relationship to existing work

- **T24 capabilities** — read-side *runtime* discovery; the config schema (§3.4)
  is its write-side sibling; keep them consistent.
- **T48 backend discovery** — the enumeration layer the config API sits on;
  unowned; §6.
- **T29 residency default / T30 dev toggles** — §3.7; the config API is how users
  deviate from the T29 default; T30's power-user "out" is a subset of this.
- **T17 access control** — snapd's authorization on `snap set` is the write-side
  of the socket access-control decision; keep them coherent.
- **T31 error taxonomy** — config-change failures (component pull failed, model
  incompatible) need codes too; align with T31 rather than inventing strings.
- **T53 modelctl multi-model sync** — pending Farshid's changelog; may already
  move some of §3.4 (schema/multi-model). Confirm before building.

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
TCP/UDS read surface (or the UI's own snapd-change adapter) would present:

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
