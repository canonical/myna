# myna-audio-adapter

PipeWire/PulseAudio audio capture and STT-preparation library for the Myna dictation pipeline.

## What it does

- Enumerates audio input nodes exposed by PipeWire (primary) or PulseAudio (fallback).
- Opens a capture stream in a stateless, idempotent “ensure open” fashion.
- Converts captured audio to a configurable target format (default: 16 kHz mono S16LE).
- Resamples using the server when possible, falling back to an in-process `rubato` resampler.
- Maintains a bounded in-memory rolling buffer (default 10 s, configurable).
- Handles overrun (drop oldest + event), underrun (silence fill + event), and device loss transparently.
- Optionally applies preprocessing stages:
  - `denoise` — RNNoise-based noise suppression
  - `vad` — Silero voice-activity detection
  - `deverb` — reserved for future dereverberation
- Never persists audio to disk and requires no network access.

## Feature flags

| Feature | Default | Description |
|---|---|---|
| `pipewire` | yes | Native PipeWire backend |
| `pulse` | yes | PulseAudio fallback backend |
| `vad` | no | Silero VAD stage |
| `denoise` | no | RNNoise denoising stage |
| `async` | no | `futures::Stream` adapter |
| `test-util` | no | Expose `MockBackend` for downstream tests |

## Build

```bash
# Default features (requires libpipewire-0.3-dev and libpulse-dev on the build machine)
cargo build -p myna-audio-adapter

# Hermetic build with no system audio server dependencies
cargo build -p myna-audio-adapter --no-default-features --features test-util

# With optional preprocessing
cargo build -p myna-audio-adapter --features vad,denoise
```

## Usage

```rust
use myna_audio_adapter::{enumerate_nodes, open_stream, StreamConfig};
use std::time::Duration;

let nodes = enumerate_nodes()?;
let mut stream = open_stream(&StreamConfig::default())?;

for item in stream.read_timeout(Duration::from_millis(100))? {
    println!("{:?}", item);
}

stream.close()?;
```

## Architecture

See [docs/diagrams.md](docs/diagrams.md) for block and sequence diagrams.

## Tests

```bash
# Hermetic unit/contract tests (MockBackend)
cargo test -p myna-audio-adapter --no-default-features --features test-util

# Real PipeWire integration tests (requires a running PipeWire server)
MYNA_AUDIO_IT=1 cargo test -p myna-audio-adapter --test integration -- --ignored
```

## Examples

- `cargo run -p myna-audio-adapter --example capture_check` — latency/lifecycle conformance.
- `cargo run -p myna-audio-adapter --example preprocess_check --features vad,denoise -- <wav>` — preprocessing validation.
