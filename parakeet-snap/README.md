# parakeet-snap — Parakeet TDT int8 ONNX inference snap

Small CPU-tier speech-to-text snap: NVIDIA Parakeet TDT 0.6B v3 (25 languages,
punctuation) as an int8 ONNX export served via onnxruntime. No torch; roughly
690 MB installed (46 MB snap + 646 MB model component).

The component carries one encoder, never both: the base int8 export, or the
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
sudo snap set myna-parakeet stream-partial-cadence-seconds=1
sudo snap restart myna-parakeet.server
```

Partials are the expensive setting. On a Ryzen AI 7 350 the decode is busy
roughly 80% of the time you are speaking at the 0.5 s default and 43% at 1 s;
without them it is 3%. Lower `stream-arm-seconds` commits sooner but decodes
more often with less right context, and each extra chunk is another chance at
the framing collapse described in `server/src/myna/testbed/parakeet.py`.
