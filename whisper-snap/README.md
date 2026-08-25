# whisper-snap

Whisper speech-to-text inference snap for Myna, following the
qwen3/gemma4 inference-snap pattern. Design rationale:
[`docs/asr-inference-snap-design.md`](../docs/asr-inference-snap-design.md).

The snap serves the Myna session API (WebSocket over a Unix domain socket)
via `myna-server` — the same faster-whisper adapter the testbed harness
measures. It has **no microphone access**: clients capture audio and push
PCM frames.

**Status:** model weights ship as per-model snap components (T15) — the
service needs no network and downloads nothing at runtime. A `cpu` engine
(baked-in venv) is verified; the `nvidia-gpu` engine + `faster-whisper-cuda`
runtime component are scaffolded and need build verification on a CUDA box.
Confined clients reach the socket via the `ubustt-socket` content share
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

# Sideloaded snaps don't auto-connect interfaces before the install hook,
# so select the engine manually once:
sudo myna-whisper.whisper use-engine --auto --assume-yes
sudo snap restart myna-whisper.server
```

Watch the server: `sudo snap logs -f myna-whisper.server`. The socket appears at
`/var/snap/myna-whisper/common/run/ubustt.sock`.

Transcribe a fixture clip through the snap (from the repo root):

```shell
uv run python dev/transcribe.py \
    --socket /var/snap/myna-whisper/common/run/ubustt.sock quiet-weather
```

## Confined clients (the `ubustt-socket` slot)

The snap exposes `$SNAP_COMMON/run` (where the session socket lives) as a
writable content share so strictly-confined clients — the `myna` dictation
snap (`myna-snap/`) — can reach it:

```shell
sudo snap connect myna:backend myna-whisper:ubustt-socket
```

The socket then appears in the client at `$SNAP_DATA/backend/run/ubustt.sock`.
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

## Idle behaviour

The server unloads the model after an idle period, freeing the bulk of its
memory (and most of the GPU VRAM); the next request reloads it (you'll see a
brief "loading…" via `progress.phase`). Tune or disable it:

```shell
sudo myna-whisper.whisper set sleep-idle-seconds=600   # default 300; 0 = never unload
sudo snap restart myna-whisper.server
```

Full process/VRAM release on idle (socket activation) is blocked upstream —
`modelctl run` forks the server without passing the listening socket, so the
snap uses in-process unload for now (see `docs/asr-inference-snap-design.md`).
