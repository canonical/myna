# nemotron-snap

NVIDIA Nemotron/FastConformer speech-to-text inference snap (UbuSTT), the
GPU-native sibling of [`whisper-snap`](../whisper-snap) (one snap per model
family). It serves the same UbuSTT session API (WebSocket over a Unix domain
socket) via `myna-server --adapter nemotron` — the same adapter the testbed
harness measures — and has **no microphone access**.

Why a second snap: NVIDIA's cache-aware streaming FastConformer-RNNT is a
**natively streaming transducer** (vs Whisper's AED re-decode), so finalize
latency is far lower (~0.03 s in testbed runs), with native punctuation and an
`att-context-size` latency/accuracy dial. English-only.

**Status:** verified in strict confinement on hardware (2026-06-14) — NeMo +
torch + CUDA load, the runtime `LD_LIBRARY_PATH` resolves, and the `.nemo`
checkpoint restores and serves.

Two startup warnings are **benign, no action needed**:
- `pydub`/`ffmpeg not found` — the adapter feeds raw PCM (a numpy array)
  straight to NeMo; it never decodes audio files, so ffmpeg is off the path.
  Don't stage it (chunky dependency, zero functional gain).
- `joblib [Errno 13] Permission denied → serial mode` — confinement blocks
  joblib's shared-memory probe; serial is correct for single-session inference.

## Build

```shell
./dev/prepare.sh            # stage the myna wheel into wheels/
./dev/download-models.sh    # fetch the .nemo checkpoint into components/
snapcraft pack              # large + slow: torch + CUDA + the checkpoint
```

## Install and verify

Model + runtime are components, so sideload them in the same `snap install`:

```shell
sudo snap install --dangerous \
    ./nemotron_*.snap \
    ./nemotron+model-streaming-multi.comp \
    ./nemotron+nemo-cuda.comp

sudo snap connect nemotron:hardware-observe
sudo snap connect nemotron:opengl
sudo nemotron use-engine --auto --assume-yes
sudo snap restart nemotron.server
```

Watch it: `sudo snap logs -f nemotron.server`; the socket appears at
`/var/snap/nemotron/common/run/ubustt.sock`. Transcribe / dictate from the repo:

```shell
uv run python dev/dictate.py --socket /var/snap/nemotron/common/run/ubustt.sock
```

## Confined clients (the `ubustt-socket` slot)

`$SNAP_COMMON/run` (the session-socket dir) is exposed as a writable content
share for strictly-confined clients — the `myna` dictation snap
(`myna-snap/`):

```shell
sudo snap connect myna:backend nemotron:ubustt-socket
```

The socket then appears in the client at `$SNAP_DATA/backend/run/ubustt.sock`.
Access control is "an admin connected the plug"; identity-based control is
T17. **Note:** the slot is in `snap/snapcraft.yaml`; rebuild + reinstall the
snap to get it.

## The latency dial

```shell
sudo nemotron set att-context-size=70,0    # lowest latency
sudo nemotron set att-context-size=70,33   # most context / accuracy
sudo nemotron set att-context-size=        # NeMo default
sudo snap restart nemotron.server
```

## Idle behaviour

The server unloads the model after `sleep-idle-seconds` (default 300; `0` =
never), freeing the bulk of GPU memory; the next request reloads it. Full
process/VRAM release on idle (socket activation) is blocked upstream — see
[`docs/asr-inference-snap-design.md`](../docs/asr-inference-snap-design.md) §4.
