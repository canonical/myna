# Backend-Snap Configuration API — Design Proposal

**Date:** 2026-07-12
**Status:** Proposed — team discussion draft (feeds workstream E; ownership TBD)
**Authors:** Charles, with Claude

How the desktop (Settings UI, orchestrator, power user) reads and changes the
configuration of the ASR inference snaps — model selection, engine selection,
residency policy, per-inference tuning — and who owns that boundary.

This is a **proposal to bring to the team**, not a settled contract. It records
positions and their rationale, names the required inference-snap-side API
changes, and lists the open questions (owner, broker shape, provisioning path).
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
and a later daemon/D-Bus API converge instead of diverging. The CLI is the first
consumer of the same schema (§3.4) a GUI will later consume.

### 3.3 A privileged config broker for unprivileged UIs

An unprivileged Settings panel cannot run `sudo modelctl`. Something must mediate
root + restart behind an authorization prompt. This is the meeting's "external
root daemon that can pull new components". Options:

- **(a) polkit-wrapped `modelctl` invocations** — smallest; a polkit action per
  verb (`set`, `use-model`, `use-engine`), the UI calls through `pkexec`/a tiny
  helper, the user gets an auth prompt. No new long-running daemon.
- **(b) a small root D-Bus daemon** with a polkit policy — cleaner API, can also
  drive snapd for component installs (layer 1) and stream progress, but a new
  privileged component to own and confine.
- **(c) snapd's own configuration** (`snap set` / snapd REST) mediated — reuses
  existing infrastructure, but our config lives in modelctl's store, not snap
  config, so this needs a bridge either way.

**Recommendation:** model the broker on (b) for the *provisioning + change* verbs
(it is the natural owner of both layer-1 component pulls with progress and
layer-2 changes), but keep the **read** path unprivileged and daemon-free (§3.5).
Start with (a) if a daemon is too much for the MVP. Either way this is **not
STT-specific** — it belongs to the inference-snaps platform, which argues for
device-engineering ownership (§4).

### 3.4 Config-schema discovery — the key required API change

For a UI to render controls without hardcoding per-snap knowledge, each snap must
**advertise its configurable keys** in machine-readable form: for each key its
**type**, **allowed values / range** (enum for `model` ∈ options and engine ∈
detected-compatible; int≥0 for `sleep-idle-seconds`; enum/free for
`att-context-size`), **default**, **scope** (package/engine/user), and whether
changing it **requires a restart**.

modelctl today has the *scopes* and *values* but **no schema of the domains** —
nothing says "`sleep-idle-seconds` is a non-negative integer" or "`model` is one
of {tiny, base, small}". This is the central inference-snap-side change:

- a `modelctl describe-config` (or `config-schema` / `--format=json` on an
  existing command) that emits the key set with types/domains/defaults/scope/
  restart-required, and
- for enums whose domain is hardware-dependent (engine, model options gated by
  installed components), the schema must reflect *what is actually installable/
  selectable on this machine*, not just the manifest superset.

This is the **write-side sibling of capabilities (T24)**: capabilities describes
the running model's *runtime* abilities; the config schema describes the *knobs*
and their domains. They should be consistent (e.g. the model enum in the schema
⊇ the active model capabilities lists) but serve different consumers.
A concrete strawman of this schema (whisper-snap) is in **Appendix A**.

### 3.5 Unprivileged read, brokered write

Displaying current settings must not require root. `modelctl get` is already
unprivileged; the schema (§3.4) read must be too. So: a UI **reads** config +
schema directly (unprivileged), and **writes** through the broker (§3.3) with a
polkit prompt. This keeps the common case (show me my settings) friction-free and
puts the auth prompt only where a mutation actually happens.

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
  the power user / debugging, defaulting to auto. The schema's engine enum is the
  set of engines *compatible with detected hardware*, and each option should
  carry **why** it matched or was ruled out (the attribute that decided it —
  compute capability, microarch, CPU flag, vendor) so a UI/debug view can explain
  the selection (see Appendix A.2).
- **No capacity pre-gating anywhere.** A model that will not fit is *attempted*
  and fails **observably on the wire** (the `preparing` → terminal-error
  lifecycle; codes are T31's), never silently. T12 sizing stays as
  *guidance/defaults*, not gates.

### 3.7 Residency / idle policy is layer-2 config, exposed as intent

`sleep-idle-seconds` + idle-action (T27/T29) are config keys. The CLI keeps the
raw knob; a future Settings control should expose **intent** ("keep dictation
instantly ready" vs "free memory when idle"), never the raw seconds or the
unload mechanism (per T29/T30). The residency **default** (T29) is the product
for most users; this API is how the 5% deviate. **Coupling (T29):** the idle
default and the client capture ring-buffer depth are one decision — the buffer
must cover the worst-case cold load the policy tolerates, or pre-ready speech is
lost.

### 3.8 Component provisioning + progress (layer 1)

"Download another model" is a **component install** via snapd, driven by the
broker (§3.3 option b) or by pointing the user at Software Center / a command
(the meeting's "tell the user to go run a command?" fallback). Progress display
depends on **snapd 2.77** component-download progress; until then the honest MVP
is a spinner + "downloading…" with no percentage, or the CLI fallback. Choosing
*other inference snaps entirely* (a different family) is **out of scope here** —
that is general inference-snap discovery / Software Center territory, not
STT-specific.

## 4. Ownership (open — the meeting's first question)

Proposed split, to confirm with the team:

- **Device engineering** owns the **config broker + schema surface** (§3.3–3.5)
  and the provisioning path (§3.8) — these are the modelctl/IE108 platform,
  per-snap and **not** STT-specific; every inference snap benefits.
- **The STT team (us)** owns the **STT-specific keys** (`att-context-size`, model
  options, residency intent mapping) and the **desktop Settings integration**
  (UD129 scope) that consumes the schema.
- **Software Center integration** for choosing other inference snaps is **out of
  scope** for this spec (not STT-specific).

## 5. Required inference-snap-side API changes (summary)

1. **Config-schema discovery** (§3.4) — machine-readable keys with
   type/domain/default/scope/restart-required, domains reflecting what is
   actually selectable on this machine. *The central change.*
2. **Brokered write path** (§3.3) — polkit-gated or daemon-mediated
   `set`/`use-model`/`use-engine` so an unprivileged UI can request changes with
   an auth prompt, without running as root or shelling `sudo modelctl`.
3. **Unprivileged schema read** (§3.5) — parity with the already-unprivileged
   `modelctl get`.
4. **Component install + progress** (§3.8) — trigger/observe model-component
   downloads from a UI; progress gated on snapd 2.77.
5. **(Nice-to-have) apply without full restart** — for language/residency tuning,
   a graceful reconfigure beats a socket-dropping restart; flag as future, not
   MVP.

## 6. Open questions for the team

- **Owner** of the broker + schema — device engineering (§4)?
- **Broker shape** — polkit-wrapped CLI (a), root D-Bus daemon (b), or snapd
  config bridge (c) (§3.3)?
- **Provisioning home** — same broker, or Software Center / snapd directly (§3.8)?
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
- **T17 access control** — the broker's polkit policy is the write-side of the
  socket access-control decision; keep them coherent.
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
before calling the broker.

Common fields on every key:

- `key` — the modelctl key (or the selector verb's target).
- `title` / `description` — human-facing (i18n is the UI's problem).
- `type` — `enum` | `integer` | `boolean` | `string`.
- `scope` — `package` | `engine` | `user` | `selector` (the `use-model`/
  `use-engine` verbs are modelled as `selector` keys, not plain `set`).
- `default`, `current` — the manifest default and the live value.
- `restart_required` — does applying this drop the socket (§5.5)?
- `privileged` — does the **write** need the broker/polkit (§3.5)? (Reads never
  do.)
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
  flags/CPUID — *not* a VRAM/capacity gate. `matched_on`/`requires` let a UI or
  debug view explain *why* an engine was chosen or ruled out (e.g. "needs compute
  capability ≥7.0").
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

When a write selects a not-yet-`installed` option (or the user adds a model),
the broker drives the snapd component install and streams progress. Strawman
events (broker → UI), progress gated on snapd 2.77 (§3.8):

```json
{ "type": "provision.progress", "snap": "whisper", "component": "model-small", "phase": "downloading", "done_bytes": 261881856, "total_bytes": 524288000 }
{ "type": "provision.progress", "snap": "whisper", "component": "model-small", "phase": "installing" }
{ "type": "provision.done",     "snap": "whisper", "component": "model-small" }
{ "type": "provision.error",    "snap": "whisper", "component": "model-small", "code": "download_failed", "message": "network unreachable" }
```

Pre-2.77 (no byte progress) the honest fallback is `phase` transitions only
(`downloading` → `installing` → done) with an indeterminate spinner. Error
`code`s align with T31, not ad-hoc strings.
</content>
</invoke>
