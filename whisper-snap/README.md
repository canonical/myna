# whisper-snap

Whisper speech-to-text inference snap (UbuSTT), following the
qwen3/gemma4 inference-snap pattern. Design rationale:
[`docs/asr-inference-snap-design.md`](../docs/asr-inference-snap-design.md).

The snap serves the UbuSTT session API (WebSocket over a Unix domain socket)
via `myna-server` — the same faster-whisper adapter the testbed harness
measures. It has **no microphone access**: clients capture audio and push
PCM frames.

**Status:** model weights ship as per-model snap components (T15) — the
service needs no network and downloads nothing at runtime. A `cpu` engine
(baked-in venv) is verified; the `nvidia-gpu` engine + `faster-whisper-cuda`
runtime component are scaffolded and need build verification on a CUDA box.
Socket access control for confined clients is T14c/T17.

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
    ./whisper_*.snap \
    ./whisper+model-tiny.comp \
    ./whisper+model-base.comp \
    ./whisper+model-small.comp
# (./whisper+faster-whisper-cuda.comp is the GPU stack — only on a CUDA box.)

sudo snap connect whisper:hardware-observe
sudo snap connect whisper:opengl   # if not auto-connected

# Sideloaded snaps don't auto-connect interfaces before the install hook,
# so select the engine manually once:
sudo whisper use-engine --auto --assume-yes
sudo snap restart whisper.server
```

Watch the server: `sudo snap logs -f whisper.server`. The socket appears at
`/var/snap/whisper/common/run/ubustt.sock`.

Transcribe a fixture clip through the snap (from the repo root):

```shell
uv run python dev/transcribe.py \
    --socket /var/snap/whisper/common/run/ubustt.sock quiet-weather
```

## Model selection

```shell
whisper list-models               # tiny / base / small
sudo whisper use-model base       # installs the model component, restarts server
whisper show-engine               # active engine + model options
```

Switching a model installs that model's component (weights are already in the
snap revision); nothing is fetched from the network at runtime.
