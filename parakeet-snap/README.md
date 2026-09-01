# parakeet-snap — Parakeet TDT int8 ONNX inference snap

Small CPU-tier speech-to-text snap: NVIDIA Parakeet TDT 0.6B v3 (25 languages,
punctuation) as an int8 ONNX export served via onnxruntime. No torch; roughly
690 MB installed (46 MB snap + 646 MB model component).

The component carries the maxstack encoder only (13% faster encode, 148 MB
smaller than the base export it is built from). Nothing falls back to the base
encoder at runtime, so the component must ship `libqsilu.so` beside it.

Streaming is enabled by default: SilenceCut emits committed chunks at pauses.
It does not emit unstable partials.

## Build

```bash
./dev/prepare.sh
./dev/download-models.sh
snapcraft pack
```

When the component's file list changes (not its contents), run `snapcraft
clean model-components` before packing: craft keeps staged files that no longer
exist in `components/`, so an incremental repack ships them anyway.

`download-models.sh` fetches the pinned upstream export into the model cache
and stages the component from it. The maxstack encoder is derived from that
export rather than downloaded, so on a fresh machine the first run fetches and
then stops with the two commands that build it; run them and re-run the fetch:

```bash
ORT_INCLUDE=/path/to/onnxruntime-linux-x64-<ver>/include ../dev/parakeet/qsilu/build.sh
cd ../server && uv run python ../dev/parakeet/build_maxstack_encoder.py \
    --model-dir ~/.cache/myna/models/parakeet-tdt-0.6b-v3-int8
```

## Install

```bash
sudo snap install --dangerous \
    ./myna-parakeet_*.snap \
    ./myna-parakeet+model-parakeet-int8.comp
```

The snap is CPU-only and does not require `hardware-observe`. Session socket:

```text
/var/snap/myna-parakeet/common/run/ubustt.sock
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
