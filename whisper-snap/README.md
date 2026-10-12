# whisper-snap

Whisper speech-to-text inference snap for Myna, following the common Myna
inference-snap pattern.

The snap serves the Myna session API (WebSocket over a Unix domain socket)
via `myna-server` — the same faster-whisper adapter the testbed harness
measures. It has **no microphone access**: clients capture audio and push
PCM frames.

**Status:** model weights ship as per-model snap components (T15) — the
service needs no network and downloads nothing at runtime. A `cpu` engine
(baked-in venv) and the `nvidia-gpu` engine with its `gpu-nvidia` runtime
component are both verified, the GPU one on an RTX 4080 Laptop GPU. An
experimental `amd-npu` engine packages FastFlowLM 1.0.8, HRX, and the
Whisper-v3-turbo NPU2 weights for AMD XDNA2 (`1022:17f0`). It requires
snapd 2.76's preview `accel` interface and still needs hardware validation.
Confined clients reach the socket via the `inference-provider` content share
(T14c, below); identity-based access control remains T17.

## Build

```shell
./dev/prepare.sh            # stage the myna wheel into wheels/
./dev/download-models.sh    # fetch CTranslate2 and pinned NPU2 weights
snapcraft pack
```

## Install and verify

Model weights are snap *components* (separate `.comp` files). On a sideload
they must be installed **in the same command** as the snap — otherwise the
install/refresh hook tries to fetch them from the store and fails
(`snap not known to the store`). Pass the model components you want; any
subset works, since the install hook keeps an installed model when the engine's
default is missing:

```shell
sudo snap install --dangerous \
    ./myna-whisper_*.snap \
    ./myna-whisper+model-tiny.comp \
    ./myna-whisper+model-base.comp \
    ./myna-whisper+model-small.comp
# Optional hardware runtimes/models:
#   ./myna-whisper+gpu-nvidia.comp
#   ./myna-whisper+fastflowlm-hrx.comp
#   ./myna-whisper+model-large-v3-turbo-npu.comp

sudo snap connect myna-whisper:hardware-observe
sudo snap connect myna-whisper:opengl   # if not auto-connected
sudo snap connect myna-whisper:accel    # AMD NPU; auto-connects on supporting snapd

# Sideloaded snaps don't auto-connect interfaces before the install hook,
# so select the engine manually once:
sudo myna-whisper.whisper use-engine --auto --assume-yes
sudo snap restart myna-whisper.server
```

Watch the server: `sudo snap logs -f myna-whisper.server`. The socket appears at
`/var/snap/myna-whisper/common/share/provider/myna.sock`.

Transcribe a fixture clip through the snap (from the repo root):

```shell
uv run python dev/transcribe.py \
    --socket /var/snap/myna-whisper/common/share/provider/myna.sock quiet-weather
```

## Confined clients (the `provider` slot)

The snap exposes `$SNAP_COMMON/share/provider` (the session socket and the
`provider.env` naming it) as a writable content share so strictly-confined clients — the `myna` dictation
snap (`myna-snap/`) — can reach it:

```shell
sudo snap connect myna:backend myna-whisper:provider
```

The share then appears in the client as `$SNAP_DATA/backend/provider/`, holding
`provider.env` and `myna.sock` (snapd suffixes `-2`, `-3` for further providers).
Access control is "an admin connected the plug"; identity-based control is
T17. Unconfined clients keep using the socket path directly.

## Model selection

```shell
myna-whisper.whisper list-models               # options depend on active engine
sudo myna-whisper.whisper use-model base       # installs the model component, restarts server
myna-whisper.whisper show-engine               # active engine + model options
```

Switching a model installs that model's component (weights are already in the
snap revision); nothing is fetched from the network at runtime.

The AMD NPU engine is batch-only. FastFlowLM listens on `127.0.0.1:52625`
inside the daemon's confinement; Myna remains exposed only through the usual
Unix socket. The runtime and NPU weights are separate components so CPU and
NVIDIA installations do not carry them.

FastFlowLM also needs to raise the service's locked-memory limit. Connect the
manual `process-control` interface before selecting the NPU engine; automatic
selection will ignore the engine until both required interfaces are connected:

```shell
sudo snap connect myna-whisper:process-control
sudo snap connect myna-whisper:accel
sudo myna-whisper.whisper use-engine amd-npu
```

## Compute precision

`show-model` reports the precision of the packaged weights. The five
CTranslate2 model components contain FP16 weights; the NPU2 component is Q4_1.
Runtime precision for the CPU/GPU engines is a separate setting:

```shell
myna-whisper.whisper get compute-type
sudo myna-whisper.whisper set compute-type=auto
sudo myna-whisper.whisper set compute-type=int8
sudo myna-whisper.whisper set compute-type=float32
```

`set` restarts the service unless `--no-restart` is passed. On the CPU engine,
`auto` selects the measured per-model default: INT8 for `tiny`, FP32 for
`base` and `small`. Explicit `int8` favors speed and `float32` favors accuracy.
On this CPU, CTranslate2 resolves `int8` to `int8_float32`: linear and embedding
weights use INT8 while the remaining computation uses FP32. The effective type
is logged when the model loads:

```shell
sudo snap logs myna-whisper.server | grep 'effective CTranslate2 compute type'
```

`int8_float32` may also be selected explicitly, though it is equivalent to
`int8` on supported CPUs.

## Idle behaviour

The server unloads the model after an idle period, freeing the bulk of its
memory (and most of the GPU VRAM); the next request reloads it (you'll see a
brief "loading…" via `progress.phase`). Tune or disable it:

```shell
sudo myna-whisper.whisper set sleep-idle-seconds=600   # default 300; 0 = never unload
sudo snap restart myna-whisper.server
```

Full process/VRAM release on idle (socket activation) is blocked upstream:
`modelctl run` forks the server without passing the listening socket, so the
snap uses in-process unload for now.

FastFlowLM does not currently expose an ASR unload operation, so the
experimental AMD NPU engine remains resident and does not inherit the shared
`sleep-idle-seconds` setting.
