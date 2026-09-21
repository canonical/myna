# parakeet-snap - Parakeet TDT ONNX inference snap

NVIDIA Parakeet TDT 0.6B v3 (25 languages, punctuation) served via onnxruntime,
no torch. Two engines, picked by hardware at install:

- `cpu` - an int8 export. Roughly 812 MB installed (46 MB snap + 766 MB model
  component).
- `nvidia-gpu` - a fp32 export of NVIDIA's checkpoint on onnxruntime's
  CUDA provider, with the CUDA runtime as its own component. See
  [NVIDIA GPU engine](#nvidia-gpu-engine).

Streaming is enabled by default: SilenceCut emits committed chunks at pauses.
It does not emit unstable partials.

## Build

```bash
make snap-parakeet
```

That stages `components/` from the pinned upstream export in the model cache.
If a pack ever drops a file from the component, run `snapcraft clean
model-components` first: craft keeps staged files that no longer exist in
`components/` and packs them anyway.

## Install

```bash
sudo snap install --dangerous \
    ./myna-parakeet_*.snap \
    ./myna-parakeet+model-parakeet-int8.comp
```

## NVIDIA GPU engine

The GPU graphs are exported here from NVIDIA's `.nemo` checkpoint rather than
downloaded. The build runs `dev/parakeet/export_parakeet_onnx.py` when the model
cache lacks them; it needs NeMo and torch, which it installs into its own uv
environment (about 2 GB, locked in `export_parakeet_onnx.py.lock`), and a 2.4 GB
checkpoint download.

Needs an NVIDIA driver of 580 or later (CUDA 13) and a Turing or newer GPU. The
engine is matched on the NVIDIA vendor id alone, so an older card or driver is
selected anyway and the server then refuses to start, naming the CUDA provider.

```bash
sudo snap install --dangerous \
    ./myna-parakeet_*.snap \
    ./myna-parakeet+model-parakeet-int8.comp \
    ./myna-parakeet+model-parakeet-fp32.comp \
    ./myna-parakeet+onnxruntime-cuda.comp
sudo snap connect myna-parakeet:hardware-observe
sudo snap connect myna-parakeet:opengl
sudo myna-parakeet.parakeet use-engine --auto --assume-yes
```

A sideload does not auto-connect `hardware-observe`, so the install hook
selects `cpu`; `use-engine --auto` re-scores once it is connected.

One model, `parakeet-tdt-0.6b-v3-fp32` (default and only option).

No fp16 model ships: the naive `onnxruntime.transformers.float16` conversion
measured slower than fp32 on fresh window lengths and lost accuracy on long
windows. See `dev/parakeet/export_parakeet_onnx.py`.

## Streaming cadence

Two independent things: when text becomes **committed** (final, injectable),
and how often the not-yet-committed audio is shown as **unstable** text a
client can render as preedit.

Committing:

- `stream-arm-seconds=15` — audio required before a pause can commit
- `stream-silence-cut-seconds=0.5` — pause length that commits
- `stream-force-cut-seconds=60` — maximum uncommitted window

Showing:

- `stream-partial-cadence-seconds=2` — how often the uncommitted window is
  re-decoded for display; `0` shows nothing until the first commit
- `stream-partial-tail-seconds=0` — `0` decodes the whole uncommitted window;
  a cap decodes only the last N seconds, which costs less and shows less

At the defaults the first words appear about 2 s in, while the first committed
segment still waits for the arm. Partials cannot change committed text —
measured identical with them on and off — so the dials are independent:

```bash
sudo myna-parakeet.parakeet set stream-partial-cadence-seconds=0.5
sudo snap restart myna-parakeet.server
```

Partials are the expensive setting, and what one costs grows with how long you
have been speaking without a pause: the whole uncommitted window is re-decoded
each time, and that window runs to `stream-force-cut-seconds`. On a Ryzen
AI 7 350 the decode is busy roughly 20-30% of the time you are speaking at the
2 s default and 50% at 0.5 s; without partials it is 3%. The server will not
let the display outrun the audio whatever you set - it spaces ticks out when
one turns out expensive, so a low cadence buys a faster first word and more
CPU, never a session that falls behind. Lower `stream-arm-seconds` commits
sooner but decodes more often with less right context, and each extra chunk is
another chance at the framing collapse described in
`server/src/myna/testbed/parakeet.py`.
