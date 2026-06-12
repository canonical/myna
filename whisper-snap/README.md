# whisper-snap

Whisper speech-to-text inference snap (UbuSTT), following the
qwen3/gemma4 inference-snap pattern. Design rationale:
[`docs/asr-inference-snap-design.md`](../docs/asr-inference-snap-design.md).

The snap serves the UbuSTT session API (WebSocket over a Unix domain socket)
via `myna-server` — the same faster-whisper adapter the testbed harness
measures. It has **no microphone access**: clients capture audio and push
PCM frames.

**Skeleton status (T14b):** model weights are downloaded into `$SNAP_COMMON`
on first use (hence the `network` plug). The production path is snap
components per model (T15). Socket access control for confined clients is
T14c/T17.

## Build

```shell
./dev/prepare.sh        # stage the myna wheel into wheels/
snapcraft pack
```

## Install and verify

```shell
sudo snap install --dangerous ./whisper_*.snap
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
sudo whisper use-model base       # switches and restarts the server
whisper show-engine               # active engine + model options
```

First use of a new model downloads its weights (CTranslate2 conversions from
`Systran/faster-whisper-*`, MIT) into `$SNAP_COMMON/huggingface`.
