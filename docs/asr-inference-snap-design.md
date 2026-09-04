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

- Socket at `$SNAP_COMMON/run/ubustt.sock`; config key `ws.unix-socket`
  replaces `http.port`/`http.host` (since modelctl v2.0.0-beta.12 that is the
  key `status` builds a ws+unix entrypoint from; earlier packaging used the
  snap-private `socket.path`, invisible to `status`).
- Use snapd **socket activation** (`sockets:` with `listen-stream`) so the
  daemon starts on first connection, complementing idle unload — together
  they give IE114's warm/cold model lifecycle with near-zero idle cost.
- Client access (desktop orchestrator snap or deb) needs a decision shared
  with T17: content interface exposing the socket directory vs. plain file
  permissions on a world-readable socket + polkit-style identity checks
  (IE114 access-control comments). Out of scope here; T17 owns it.
  **Update (2026-07-22, feature 005 / T57):** the confined-client half is
  now settled in favor of the **content interface**: each inference snap
  slots `$SNAP_COMMON/run` as a writable content share (`ubustt-socket`),
  which is precisely the case snapd's content interface supports named
  sockets for (its writable-share AppArmor rules exist "for using named
  sockets within the exported directory"). The `myna` client snap plugs it
  at `$SNAP_DATA/backend`. What remains with T17 is the *identity* half
  (polkit-style checks on who may talk to the socket once connected) — the
  share itself is admin-gated only.
- `runtime.yaml` `servers:` entry declares the endpoint, e.g.
  `ubustt: {protocol: ws+unix, base-path: /v1}`. **Resolved upstream
  (v2.0.0-beta.12):** `ws+unix` is a first-class protocol in `status` /
  entrypoints, so no schema flagging is needed anymore.

### 3.3 Confinement: no microphone

Under the audio-push model the client owns PipeWire capture, so the snap
needs **no audio interfaces at all** — a meaningful privacy story: the STT
snap is incapable of recording. Plugs reduce to `hardware-observe` + `opengl`
(hooks/CLI/GPU) + `network-bind`; webui and `chat` features are dropped
entirely.

Note (verified empirically on T14b): `network-bind` is required even with no
TCP endpoint — snapd's seccomp policy gates the `listen()` syscall behind it
regardless of address family, so a pure Unix-socket server still needs it.

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
session request. Snap config (via `modelctl`, IE108): `ws.unix-socket`,
`sleep-idle-seconds`, `verbose`, and engine `configurations` for inference
tuning (`compute-type`, `beam-size`, streaming strategy knobs from T08).

## 4. Known gaps / things to flag upstream

**Upstream feedback (2026-07-08, Farshid, testbed review):** items 2–4 are
"surely doable"; item 1 is withdrawn — see its note. He also verified the
whisper snap uses the NVIDIA GPU, and flagged that our snaps' modelctl wiring
needs minor updates for upstream's improved multi-model support (plan T53).

1. **VRAM-aware model gating** — **withdrawn (2026-07-08)**: upstream is
   removing vRAM gating from the *engine* level too, so don't raise the
   per-model version. Farshid's reasons, recorded: total/available vRAM at
   install time is stale by startup (other applications take chunks); some
   runtimes split a model across vRAM and CPU so a single number gates
   wrongly (except MoE models that must fit at once); embeddings/context add
   vRAM beyond the weights; and NVIDIA unified-memory platforms intentionally
   don't report vRAM at all. **Our consequence:** no pre-gating anywhere —
   attempt the load and **fail observably**: a load failure surfaces on the
   wire through the existing lifecycle (`preparing` → terminal error; codes
   are T31's), never a silent stall, and idle-unload (T27) bounds
   steady-state pressure. T12 measurements stay useful as documented
   guidance/defaults, not gates.
2. **UDS server declaration** in `runtime.yaml` `servers:` (see 3.2).
   **Resolved upstream (v2.0.0-beta.12, PR #412):** `ws+unix` is now a
   supported status protocol and the status output carries an `entrypoints`
   dictionary (replacing `endpoints`). Earlier finding, for the record:
   `modelctl status` (v2.0.0-beta.1) fails with
   `unsupported protocol "ws+unix" for server "ubustt"`.
3. **Capabilities discovery** is CLI-only for now (matches IE114's
   "configuration via CLI initially"); the network API remains open (T24).
4. **Socket activation is blocked by `modelctl run`** (found T28, 2026-06-14).
   snapd socket activation passes the listening socket as fd 3 to the daemon
   (advertised via `LISTEN_FDS`/`LISTEN_PID`), so the daemon can exit on idle
   and be relaunched on the next connection — full process/VRAM release. But
   `modelctl run` (`run.go`) launches the server with Go's `exec.Command(...).
   Run()`, which **forks a child** (so its PID ≠ `LISTEN_PID`) and does **not**
   set `ExtraFiles` (so fd 3 is never inherited). Either alone breaks the
   handoff. Until `modelctl run` either `syscall.Exec`s the server or forwards
   the listening fd + `LISTEN_PID`, the snap uses **in-process idle-unload**
   (`--idle-action unload`, T27) instead: frees the model weights (the bulk)
   after `sleep-idle-seconds`, leaving only the runtime process + CUDA context.
   Still fork-based as of v2.0.0-beta.12 (checked during T53); the ask stands:
   raise with the inference-snaps-cli team.

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
4. **T15** — model weights as components + `nvidia-gpu` engine. **Done (CPU)
   / scaffolded (GPU):**
   - Model weights now ship as per-model snap components
     (`model-{tiny,base,small}`), fetched into `whisper-snap/components/` by
     `dev/download-models.sh` and routed into components at pack time. The
     server loads from `MODEL_DIR=$SNAP_COMPONENTS/<id>`; the `network` plug
     is dropped — no runtime download.
   - Runtime-delivery decision: the **cpu venv stays baked into the base
     snap** (the working T14b path), while the **CUDA stack ships as the
     `faster-whisper-cuda` component** (myna[whisper] + ctranslate2 + NVIDIA
     cuBLAS/cuDNN pip libs under `site-packages/`, exposed via
     PYTHONPATH/LD_LIBRARY_PATH and run by the base `python3` — relocatable,
     sidestepping the venv-relocation fallback flagged in §3.1).
   - `nvidia-gpu` engine + `faster-whisper-cuda` runtime are written but
     **UNVERIFIED on hardware** — build + auto-selection must be confirmed on
     a CUDA box. Larger GPU-tier weights (medium, large-v3, distil-large-v3)
     are deferred until T12 sizes them (see VRAM gap below).

## 7. Packaged adapter comparison (measured, feature 008)

Working draft — feature 008 sweep data; extend per tier via `dev/matrix.py`.
Numbers from `results/streaming-watermarks.json` on the long-stream corpus
(26–33 s same-speaker streams). WER across rows is comparable in kind but not
in corpus size — treat ±1 pp as noise.

| Snap | Model | Engine | Langs | Streaming strategy | WER batch | WER stream | TTFU | TTFC | Finalize |
|------|-------|--------|-------|--------------------|-----------|------------|------|------|----------|
| whisper | faster-whisper tiny (CPU) | CTranslate2 | multi | local-agreement re-decode (1 s cadence) | 4.8 % | 7.2 % (+2.4 pp) | 1.4–1.5 s | 2.4–3.5 s | 0.36 s |
| nemotron | FastConformer hybrid large streaming multi | NeMo, CUDA | en | native cache-aware transducer | 0.0 % | 0.0 % (batch parity) | 2.4 s | 4.5 s (length-independent) | 0.06 s |
| parakeet | Parakeet TDT 0.6B v3 int8 (murmure weights) | onnxruntime | 25 | chunked commit (SilenceCut; no partials) | 4.4 / 0.0 % | 2.2 / 0.0 % (≤ batch) | none | 17.8–20.4 s | 0.42 s |
| sherpa | k2 FastConformer transducer 480 ms int8 | sherpa-onnx | en | native partials + rule endpointing | 4.4 / 3.9 % | 6.7 / 6.5 % (+2.2–2.6 pp) | 1.3 s | 12.0–20.5 s | 0.01 s |
| qwen-c | Qwen ASR (C runtime) | external C lib | — | batch only | — | — | — | — | — |
| fake | scripted (no model) | — | — | wire/contract regression fixture | — | — | — | — | — |

Built component sizes: whisper model components tiny 73 MB / base 140 MB /
small 462 MB, CUDA runtime component 1.7 GB; server snap 161 MB. The other
snap payloads are not built on this machine.

Reading the table:

- **Accuracy leaders**: parakeet (chunked decode-once — no re-decode
  right-context tax) and nemotron (frame-once native streaming — batch
  parity). Whisper re-decode pays ~+2.4 pp at commit boundaries.
- **Latency leaders**: sherpa for TTFU (1.3 s) and finalize (0.01 s);
  nemotron for length-independent TTFC.
- **Trade-off shorthand**: whisper = multilingual + tunable, moderate WER
  cost; nemotron = best GPU-tier profile but en-only and heavy (NeMo);
  parakeet = best CPU accuracy, no partials; sherpa = fastest partials,
  no punctuation, endpoint-dependent commits.

Hardware tiers: whisper rows are x86-64 CPU (whisper-tiny); nemotron row is
RTX 4080 Laptop GPU. Per-tier watermarks live in
`results/streaming-watermarks.json`; the streaming/batch default gate is
`results/streaming-tiers.json`.
