# ASR Inference Snap Design — Whisper Snap (T13)

**Date:** 2026-06-13
**Status:** Proposed
**Authors:** Claude, with Charles

Design note for the first ASR inference snap, based on a study of
`reference/qwen3-snap`, `reference/gemma4-snap`, and
`reference/inference-snaps-cli`. Decision already taken: **one snap per model
family** (matching how qwen3/gemma4 are per-family), and **Whisper first**.

## 1. How the existing inference snaps work

There are two generations of the engine schema; gemma4 carries the newer one
and is the template to copy.

**qwen3 (v1):** flat `engines/<name>/engine.yaml` with `devices` matching
rules, `memory`/`disk-space` gates, and a hardcoded `components` list (runtime
+ model). One model per engine.

**gemma4 (v2):** engine, runtime, and model are decoupled manifests, which is
exactly the shape an ASR snap needs (one engine serving several model sizes):

- `engines/<name>/engine.yaml` — hardware `devices` rules (CPU arch/flags,
  PCI vendor-id, GPU microarchitecture/compute-capability/VRAM), a `runtime`
  reference, `model: {default, options}`, and `configurations` (key/value
  defaults like `sleep-idle-seconds`).
- `runtimes/<name>/runtime.yaml` — `servers:` (protocol + base-path),
  `environment` (PATH/LD_LIBRARY_PATH into component dirs), `components`
  (the runtime binaries, e.g. llamacpp-cuda).
- `models/<id>/model.yaml` — `disk-size`, `capabilities`, `components`
  (weight files), `environment` (MODEL_FILE, MODEL_NAME).
- `engines/<name>/server` — small bash script: read config via
  `modelctl get`, exec the runtime binary with the model env vars.

**Runtimes and weights are snap components** (snapd ≥ 2.68), installed on
demand — the base snap is small and `modelctl use-engine`/`use-model` pulls
only what the selected combination needs.

**`modelctl`** (from inference-snaps-cli, IE108) provides, for free:
config get/set/unset backed by snapctl; engine scoring and auto-selection
against detected hardware (`pkg/selector`: device match, memory/disk gates);
model switching; `run` (waits for components, composes the engine+runtime+
model environment, execs the server); `status`; tab completion.

**Lifecycle:** the `install` hook sets package config defaults and runs
`modelctl use-engine --auto` if `hardware-observe` is connected;
`post-refresh` runs `use-engine --fix`. The `server` snap daemon runs
`scripts/server.sh` → resolves the active engine → `modelctl run --
$SNAP/engines/$engine/server`. Idle resource usage is handled by the server's
`--sleep-idle-seconds` flag (llama-server feature), fed from config — this
maps directly onto IE114's "model stays in memory for a configurable period".

## 2. What the Whisper snap reuses unchanged

- The whole engine/runtime/model manifest machinery and `modelctl` (IE108
  compliance comes free).
- Component-based delivery of runtimes and weights.
- Install/post-refresh hooks for hardware-based engine selection.
- The `sleep-idle-seconds` config pattern and the versioning scheme
  (CLI version + git hash).
- The `status` content-interface slot (useful later for the Settings UI /
  Speech Orchestrator to observe engine/model state).

## 3. What is different for ASR

### 3.1 The server (the real work)

llama-server is replaced by a **UbuSTT session server**: accepts a session,
receives pushed PCM frames, emits `transcription.progress/final/done/error`
(the `myna.core` contract; wire transport per IE114 once settled — current
direction WebSocket over UDS).

Recommendation: **the testbed adapter is the server.** The faster-whisper
adapter built in T07/T08 already implements `SttService`; wrapping it behind
the real transport (T16) and an entry point (`python -m myna.server`) gives
the snap its server binary. One codebase, measured by the harness, shipped by
the snap — no drift between what we evaluate and what we ship.

Consequence: the runtime components contain a **Python runtime + vendored
venv** (faster-whisper, CTranslate2, myna) rather than a single C++ binary:

- `faster-whisper-cpu` component: relocatable venv with CPU CTranslate2.
- `faster-whisper-cuda` component: venv with CUDA CTranslate2 + cuBLAS/cuDNN
  libs (host driver via `opengl` interface, as the reference snaps do).

Fallback if snap-packaging a Python venv proves painful: **whisper.cpp** has
a C/C++ server like llama.cpp and would slot into the reference pattern
verbatim (Canonical already maintains llama.cpp builds). Cost: diverges from
the testbed adapter code and from the LocalAgreement streaming prior art.
Keep this as plan B, decide after T14a.

### 3.2 Endpoint: Unix socket, not TCP

The reference snaps bind HTTP on localhost TCP (`http.port`/`http.host`
config). UbuSTT is a UDS service. Proposal:

- Socket at `$SNAP_COMMON/run/ubustt.sock`; config key `socket.path` replaces
  `http.port`/`http.host`.
- Use snapd **socket activation** (`sockets:` with `listen-stream`) so the
  daemon starts on first connection, complementing idle unload — together
  they give IE114's warm/cold model lifecycle with near-zero idle cost.
- Client access (desktop orchestrator snap or deb) needs a decision shared
  with T17: content interface exposing the socket directory vs. plain file
  permissions on a world-readable socket + polkit-style identity checks
  (IE114 access-control comments). Out of scope here; T17 owns it.
- `runtime.yaml` `servers:` entry declares the endpoint, e.g.
  `ubustt: {protocol: ws+unix, base-path: /v1}` — the field is free-form
  enough today; flag to the inference-snaps-cli team that a UDS server type
  is coming.

### 3.3 Confinement: no microphone

Under the audio-push model the client owns PipeWire capture, so the snap
needs **no audio interfaces at all** — a meaningful privacy story: the STT
snap is incapable of recording. Plugs reduce to `hardware-observe` + `opengl`
(hooks/CLI/GPU). `network-bind` can likely be dropped once the TCP endpoint
is gone; webui and `chat` features are dropped entirely.

### 3.4 Engine/model matrix (initial)

| Engine | Runtime | Devices | Model options (components) |
|---|---|---|---|
| `cpu` | faster-whisper-cpu (int8) | amd64/arm64 | `tiny`, `base`, `small`, `distil-small-en` |
| `nvidia-gpu` | faster-whisper-cuda (fp16/int8_float16) | vendor-id 0x10de | `small`, `medium`, `large-v3`, `distil-large-v3` |

Weights are CTranslate2 conversions (`Systran/faster-whisper-*`), MIT
licensed — redistributable as components (license files staged like the
reference snaps' NOTICE handling). `model.yaml` `capabilities` should carry
language support (`multilingual` vs `en`) — this becomes the seed of the
IE114 capabilities-discovery answer (T24).

Intel (OpenVINO whisper) is plausible later; mirror gemma4's
`openvino-model-server` engine split when it comes.

### 3.5 Configuration keys

Per-request parameters (language, prompt, timestamps) stay in the IE114
session request. Snap config (via `modelctl`, IE108): `socket.path`,
`sleep-idle-seconds`, `verbose`, and engine `configurations` for inference
tuning (`compute-type`, `beam-size`, streaming strategy knobs from T08).

## 4. Known gaps / things to flag upstream

1. **VRAM-aware model gating**: v2 `model.yaml` has `disk-size` but no
   memory/VRAM requirement; engine-level `devices` can gate on `vram` but
   per-model gating (large-v3 needs ~3 GB VRAM, tiny doesn't) has no home.
   Raise with the inference-snaps-cli team; testbed measurements (T12) will
   provide the numbers.
2. **UDS server declaration** in `runtime.yaml` `servers:` (see 3.2).
3. **Capabilities discovery** is CLI-only for now (matches IE114's
   "configuration via CLI initially"); the network API remains open (T24).

## 5. Proposed whisper-snap structure

New repo (`whisper-snap`, name parallel to qwen3-snap), once T14a exists:

```
whisper-snap/
  snap/snapcraft.yaml          # core24, strict; components for runtimes+models
  snap/hooks/{install,post-refresh}
  scripts/server.sh            # resolve engine -> modelctl run engines/<e>/server
  engines/cpu/{engine.yaml,server}
  engines/nvidia-gpu/{engine.yaml,server}
  runtimes/faster-whisper-cpu/runtime.yaml
  runtimes/faster-whisper-cuda/runtime.yaml
  models/{tiny,base,small,medium,large-v3,distil-large-v3,...}/model.yaml
  components/                  # venv build recipes + model download manifests
  download-models.sh           # hf download (resumable), HF_HOME pinned
```

The server code itself stays in the myna repo and is consumed as a wheel —
the snap repo holds packaging only, like the reference snaps hold no
llama.cpp source.

## 6. Phasing (maps to plan tasks)

1. **T14a** — `myna.server`: standalone entry point wrapping an `SttService`
   adapter behind the real transport on a UDS path. Depends on T07 (whisper
   adapter) + T16 (transport prototype). Runnable on bare metal first —
   prove the server before wrapping it in confinement.
2. **T14b** — whisper-snap skeleton: snapcraft.yaml, `cpu` engine, one small
   model component, modelctl wiring, hooks. Acceptance: install, connect
   interfaces, `modelctl use-engine --auto`, transcribe a fixture clip
   through the socket via the harness.
3. **T14c** — socket exposure/access control for confined clients (joint
   with T17).
4. **T15** — `nvidia-gpu` engine, remaining model components, VRAM gating
   follow-up.
