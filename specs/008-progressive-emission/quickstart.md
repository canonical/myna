# Quickstart: Validating Progressive Streaming Emission

**Feature**: `specs/008-progressive-emission`

Prerequisites: real corpus fetched (`dev/fetch_real_corpus.py`), model cache
populated, `client/` built (`cargo build --release`). GPU scenarios require the
NVIDIA PC.

## S1 — Whisper local-agreement

```sh
myna-server --socket /tmp/myna.sock --adapter whisper --model base --streaming
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --show-unstable \
    --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: `~` unstable lines during playback; at least one committed `»`
before the clip ends; terminal `✓` equals the concatenation of `»` lines.

Batch regression: omit `--streaming`; exactly one `»` arrives at end-of-audio.

## S2 — Nemotron native loop (GPU)

```sh
myna-server --socket /tmp/myna.sock --adapter nemotron --streaming
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --show-unstable --clip <30 s real clip>
```

Expected: continuous `~` partials; committed `»` segments at natural
boundaries; terminal `✓` within 1 s of clip end.

Validated 2026-08-04 (RTX 4080 Laptop, 30 s stream + 5 s clip): finalize
0.059 s, TTFC 4.48 s at both lengths (ratio 1.0), streaming WER == batch —
`results/streaming-watermarks.json` (`emission_008_nemotron_native`),
pattern pinned in `results/spike-s2-nemo-streaming.md`.

## S3 — Small snaps

```sh
sudo snap install --dangerous \
    myna-parakeet_*.snap myna-parakeet+model-parakeet-int8.comp
sudo snap install --dangerous \
    myna-sherpa_*.snap myna-sherpa+model-fastconformer-480ms.comp
```

Parakeet:

```sh
./client/target/release/myna-dictate \
    --socket /var/snap/myna-parakeet/common/run/ubustt.sock \
    --mode streaming --clip <real clip>
```

Expected: committed `»` chunks after the configured arm plus a pause; no `~`
partials. Default arm is 15 s; adjust with:

```sh
sudo snap set myna-parakeet stream-arm-seconds=5
sudo snap restart myna-parakeet.server
```

Sherpa:

```sh
./client/target/release/myna-dictate \
    --socket /var/snap/myna-sherpa/common/run/ubustt.sock \
    --mode streaming --show-unstable --clip <real clip>
```

Expected: continuous `~` partials and endpoint-driven committed `»` segments.
Neither snap requires `hardware-observe`.

## S4 — Watermarks and gates

```sh
dev/rebaseline-streaming-watermarks.sh
```

Expected: `results/streaming-watermarks.json` records time-to-first-unstable,
time-to-first-committed, finalize latency, WER, and commit stability per
backend×tier.

## S5 — Concluding report

`docs/interop/streaming-conclusion.md` compares accuracy, latency, footprint,
and tier coverage, and recommends a backend per hardware tier.
