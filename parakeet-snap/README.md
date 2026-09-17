# parakeet-snap - Parakeet TDT ONNX inference snap

NVIDIA Parakeet TDT 0.6B v3 (25 languages, punctuation) served via onnxruntime,
no torch. Two engines, picked by hardware at install:

- `cpu` - an int8 export. Roughly 690 MB installed (46 MB snap + 646 MB model
  component).
- `nvidia-gpu` - a fp32 export of NVIDIA's checkpoint on onnxruntime's
  CUDA provider, with the CUDA runtime as its own component. See
  [NVIDIA GPU engine](#nvidia-gpu-engine).

The int8 component carries one encoder, never both: the base int8 export, or the
maxstack rebuild of it (13% faster encode, 148 MB smaller) with `libqsilu.so`
beside it for the custom ops it calls. Nothing falls back at runtime. Sizes
above are the maxstack shape; the base one installs at 812 MB.

Streaming is enabled by default: SilenceCut emits committed chunks at pauses.
It does not emit unstable partials.

## Build

```bash
make snap-parakeet            # base encoder
make snap-parakeet-maxstack   # optimized encoder - ~13x faster encode
```

Either stages `components/` from the pinned upstream export in the model
cache. Switching encoders changes the component's file list, so run `snapcraft
clean model-components` first: craft keeps staged files that no longer exist in
`components/` and packs them anyway.

The maxstack encoder is derived from that export rather than downloaded, so it
has to be built once per machine before it can be staged:

```bash
make parakeet-maxstack-encoder
```

That fetches the pinned onnxruntime headers, builds the custom-op kernels,
downloads the LibriSpeech calibration tier (~330 MB) and runs the
requantization pass, which peaks at several GB of RSS - see
`dev/parakeet/build_maxstack_encoder.py` about running it under a memory cap
the first time.

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

One model, `parakeet-tdt-0.6b-v3-fp32` (default and only option). Its
transcripts are identical to NeMo PyTorch's (0.00% WER between them; the int8
cpu model differs by 0.73%). Measured with `myna-bench` against the installed
snap on an RTX 4080 Laptop GPU, 82-clip balanced corpus plus 5 min long-form,
2026-09-17:

| engine | mode | WER | speed | final latency median / p95 | peak VRAM |
|---|---|---|---|---|---|
| cpu int8 | batch | 1.46% | 60x | 0.143 / 0.350 s | - |
| cpu int8 | streaming | 1.54% | 6.0x | 1.38 / 7.09 s | - |
| nvidia-gpu fp32 | batch | 1.42% | 122x | 0.065 / 0.099 s | 3.8 GB |
| nvidia-gpu fp32 | streaming | 1.42% | 9.6x | 0.84 / 2.74 s | 4.6 GB |

The CUDA provider pays kernel setup for every new input length, and streaming
windows are nearly always new lengths, so GPU streaming gains less over cpu
than batch does.

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

- `stream-partial-cadence-seconds=0.5` — how often the uncommitted window is
  re-decoded for display; `0` shows nothing until the first commit
- `stream-partial-tail-seconds=0` — `0` decodes the whole uncommitted window;
  a cap decodes only the last N seconds, which costs less and shows less

At the defaults the first words appear about 0.6 s in, while the first
committed segment still waits for the arm. Partials cannot change committed
text — measured identical with them on and off — so the dials are independent:

```bash
sudo myna-parakeet.parakeet set stream-partial-cadence-seconds=1
sudo snap restart myna-parakeet.server
```

Partials are the expensive setting. On a Ryzen AI 7 350 the decode is busy
roughly 80% of the time you are speaking at the 0.5 s default and 43% at 1 s;
without them it is 3%. Lower `stream-arm-seconds` commits sooner but decodes
more often with less right context, and each extra chunk is another chance at
the framing collapse described in `server/src/myna/testbed/parakeet.py`.
