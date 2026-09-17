# whisper-snap

Whisper speech-to-text inference snap for Myna, following the common Myna
inference-snap pattern.

The snap serves the Myna session API (WebSocket over a Unix domain socket)
via `myna-server` — the same faster-whisper adapter the testbed harness
measures. It has **no microphone access**: clients capture audio and push
PCM frames.

**Status:** model weights ship as per-model snap components (T15) — the
service needs no network and downloads nothing at runtime. A `cpu` engine
(baked-in venv) is verified; the `nvidia-gpu` engine + `faster-whisper-cuda`
runtime component are scaffolded and need build verification on a CUDA box.
Confined clients reach the socket via the `inference-provider` content share
(T14c, below); identity-based access control remains T17.

## Build

```shell
./dev/prepare.sh            # stage the myna wheel into wheels/
./dev/download-models.sh    # fetch CTranslate2 weights into components/
snapcraft pack
```

## Install and verify

Model weights are snap *components* (separate `.comp` files). On a sideload
they must be installed **in the same command** as the snap — otherwise the
install/refresh hook tries to fetch them from the store and fails
(`snap not known to the store`). Pass the model components you want:

```shell
sudo snap install --dangerous \
    ./myna-whisper_*.snap \
    ./myna-whisper+model-tiny.comp \
    ./myna-whisper+model-base.comp \
    ./myna-whisper+model-small.comp
# (./myna-whisper+faster-whisper-cuda.comp is the GPU stack — only on a CUDA box.)

sudo snap connect myna-whisper:hardware-observe
sudo snap connect myna-whisper:opengl   # if not auto-connected
sudo snap connect myna-whisper:system-observe   # the server daemon needs this

# Sideloaded snaps don't auto-connect interfaces before the install hook,
# so select the engine manually once:
sudo myna-whisper.whisper use-engine --auto --assume-yes
sudo snap restart myna-whisper.server
```

`system-observe` is not optional on a sideload: CTranslate2/ONNX Runtime read
`/sys/fs/cgroup/**` and `/proc/**` for CPU topology at startup, so without it
the `server` daemon exits immediately and keeps doing so until systemd's start
limit trips — `Job for snap.myna-whisper.server.service failed because start of
the service was attempted too often`. Clearing that needs a `reset-failed`,
because systemd will not retry on its own:

```shell
sudo snap connect myna-whisper:system-observe
sudo systemctl reset-failed snap.myna-whisper.server.service
sudo snap start myna-whisper.server
```

`snap connections myna-whisper` is the quickest check — any plug showing `-` in
the Slot column is unconnected. Reach for it before `snap logs`, which is
typically empty in this failure mode (the process dies before it logs).

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
myna-whisper.whisper list-models               # tiny / base / small
sudo myna-whisper.whisper use-model base       # installs the model component, restarts server
myna-whisper.whisper show-engine               # active engine + model options
```

Switching a model installs that model's component (weights are already in the
snap revision); nothing is fetched from the network at runtime.

## Compute precision

`show-model` reports the precision of the packaged CTranslate2 weights. All
three model components currently contain FP16 weights. Runtime precision is a
separate engine setting:

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
