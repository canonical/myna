# myna

Local, private speech-to-text (dictation) for Ubuntu Desktop: activate a hotkey,
speak, and the transcribed text is injected into the app you're focused on. No
cloud, no persisted audio — everything runs offline on your machine.

The project is named for the [myna](https://en.wikipedia.org/wiki/Myna), a bird
that listens to and reproduces human speech with striking clarity.

## Try it locally in 5 minutes

You need the [Rust toolchain](https://rustup.rs), [uv](https://docs.astral.sh/uv/),
and the PipeWire build headers:

```shell
sudo apt install libpipewire-0.3-dev libclang-dev pkg-config
```

The fastest smoke test needs no model weights and no audio — the `fake` adapter
serves a scripted transcript over the in-process loopback transport:

```shell
cd server && uv sync && uv run python -m myna.testbed   # fake adapter -> a ResultRecord
```

To dictate for real, serve a model on a socket and push audio at it from the Rust
client (16 kHz mono works out of the box). Get a WAV first — either grab the
LibriSpeech corpus (`uv run python ../dev/fetch_real_corpus.py`) or use your own:

```shell
# 1. serve Whisper on a Unix socket (downloads the `base` weights on first run)
(cd server && uv sync --extra whisper && \
    uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &

# 2. dictate — from a WAV clip, or live from the microphone
cd client && cargo build --release && cd ..
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en \
    --clip corpus/real/audio/<id>.wav          # a clip (Enter to start, clip-end to stop)
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en --mic   # live mic
```

The rest of this README expands on each piece.

## What's in here

- `server/` — the **Python** side. `myna.core` is the shared session contract
  (audio, transcript events, session config, transports, the IE115 wire codec);
  `myna.server` is `myna-server`, the process the snaps ship; `myna.testbed` is
  the model-evaluation harness (adapters, corpus, metrics).
- `client/` — the **Rust** dictation client. `myna-dictate` (`myna-cli`) is the
  testbed/demo push-to-talk client; `myna-desktop` is the shipped dictation app
  (hotkey → capture → IBus injection into the focused app, with an activity
  indicator). `myna-audio` is the native PipeWire capture adapter,
  `myna-orchestrator` the session FSM, `myna-core` the wire contract.
- `extensions/myna-shell/` — a **GJS** GNOME Shell extension: a focus-safe,
  dictation indicator that runs inside the compositor and consumes state from
  `myna-desktop` over D-Bus. It defaults to a simple native-style audio meter;
  the animated wave ribbon remains selectable in extension preferences.
- `whisper-snap/`, `nemotron-snap/`, `qwen-snap/` — one inference snap per model
  family (strict-confinement packages of `myna-server` + a model + engines).
- `docs/` — architecture and design notes; `docs/project-plan.md` is the living
  task tracker.

## How it fits together

Everything speaks one **session contract** — audio-push in, transcript events
out — over WebSocket-on-a-Unix-socket. Three roles play against it:

- **Inference backend** (`myna-server` / a snap): hosts a model, listens on a
  socket, has *no* microphone — the client pushes PCM and the server rejects
  off-format audio rather than resampling. Swap models with
  `--adapter whisper|nemotron|qwen-c|fake`.
- **Dictation client** (Rust, `client/`): owns capture and the hotkey and runs
  the session FSM. `myna-dictate` is the demo (WAV/corpus/live-mic);
  `myna-desktop` is the shipped app that injects into the focused app via IBus.
- **Benchmark client** (`dev/bench.py`): replays a corpus through a backend and
  scores WER/CER offline, so accuracy work stays out of the dictation hot path.

The wire is a **selectable dialect** — the internal `transcription.*` vocabulary
or the OpenAI-Realtime-shaped **IE115** names — and both ends translate at the
edge, so neither the models nor the FSM change when the dialect does
(`docs/architecture/ie115-wire.md`).

## Development environment

### Workshop (recommended)

A [Canonical Workshop](https://ubuntu.com/workshop/docs) definition in
[`.workshop/`](.workshop/) gives CI's core Rust, PipeWire, GJS, and `uv`
toolchain in one reproducible environment. GPU/model extras and snap-build
coverage remain tracked as T55 in `docs/project-plan.md`.

```shell
workshop launch myna
workshop shell myna                                                # a shell, project at /project
workshop exec myna -- bash -lc 'cd /project/client && cargo test'  # or one command
```

To capture from a real host microphone inside the workshop, connect host audio:
`workshop connect myna/pipewire:sound :custom-device` (the integration tests
don't need it — they build their own virtual-audio graph).

### Manual setup

Install the prerequisites listed under [Try it locally](#try-it-locally-in-5-minutes).
Then, for the Python side, `uv` owns a project virtualenv (`server/.venv/`) that
it keeps matching `server/pyproject.toml` + `server/uv.lock`:

```shell
cd server
uv sync                                     # base + dev (fake adapter, contract tests)
uv sync --extra whisper                     # + Whisper
uv sync --extra whisper --extra nemotron    # + both real adapters
```

`uv sync` is declarative — it makes `.venv` match *exactly* what you request, and
syncing with fewer extras prunes ones installed earlier. A `ModuleNotFoundError`
for `faster_whisper` / `nemo` just means that extra isn't synced.

| Extra      | Pulls in                                      | For                    |
|------------|-----------------------------------------------|------------------------|
| `whisper`  | faster-whisper (CTranslate2)                  | the Whisper adapter    |
| `nemotron` | `nemo_toolkit[asr]` + torch + CUDA (multi-GB) | the Nemotron adapter   |

The `qwen-c` adapter needs **no** extra — it's a pure-C engine reached via
ctypes. Point it at the shared library and a local model dir:
`QWEN_ASR_LIB=/snap/qwen/current/lib/libqwen_asr.so uv run myna-server --adapter
qwen-c --model ../qwen-snap/components/Qwen3-ASR-0.6B --socket /tmp/myna.sock`.

Model **weights** are separate from the code: they download to `HF_HOME` on first
use of an adapter (verify offline with `HF_HUB_OFFLINE=1`).

## Common commands

```shell
cd server
uv run pytest                                   # offline suite: contract + adapter logic
uv run python -m myna.testbed                   # demo: fake adapter over loopback
uv run python ../dev/generate_fixtures.py       # synthetic corpus -> server/fixtures/
uv run python ../dev/fetch_real_corpus.py       # real LibriSpeech corpus -> corpus/real/

# serve a real adapter, then talk to it:
uv run myna-server --adapter nemotron --socket /tmp/myna.sock   # or whisper | qwen-c
uv run python ../dev/capabilities.py --socket /tmp/myna.sock    # what can this server do?
uv run python ../dev/transcribe.py --socket /tmp/myna.sock quiet-weather   # a fixture clip
```

Both corpora are generated, not committed — run the builders once before
corpus-backed runs.

## The dictation client

`myna-dictate` owns capture and drives the session FSM against any running
backend:

```shell
cd client && cargo build --release && cd ..
(cd server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &

# real-time push-to-talk from a WAV clip:
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en \
    --clip corpus/real/audio/<id>.wav

# over the OpenAI-Realtime IE115 wire — same FSM, different dialect:
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --language en --clip corpus/real/audio/<id>.wav

# live microphone via the native PipeWire backend:
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en --mic
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en --mic \
    --target alsa_input.pci-0000_c1_00.6.HiFi__Mic2__source   # a specific node.name

# list input devices live (stable node.name + label):
./client/target/release/myna-dictate --list-devices
```

## Streaming mode

By default dictation is **batch**: text appears when the utterance ends. A
backend started with `--streaming` also emits **committed segments**
progressively as you speak — each is append-only (never retracted). Provisional
hypothesis text (`unstable`) is never injected; it only displays if you opt in.

```shell
# 1. serve an adapter in streaming mode
(cd server && uv run myna-server --adapter whisper --model tiny \
    --socket /tmp/myna.sock --streaming) &

# 2. dictate — committed segments print as » lines before the final ✓
#    (force --mode streaming: the auto default resolves to batch on hardware
#    whose baseline RTF ≥ 1.0, e.g. CPU-only whisper-tiny)
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --clip corpus/real/audio/<id>.wav

# other display modes:
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode batch --clip corpus/real/audio/<id>.wav      # no » lines, only ✓
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --show-unstable --clip corpus/real/audio/<id>.wav   # + ~ lines
```

- **Tier gate**: `--mode auto` (the default) resolves against measured RTF
  baselines in `results/streaming-tiers.json` — streaming only when the model's
  RTF < 1.0 on this hardware; batch otherwise (including unmeasured hardware).
  The preference persists in `~/.config/myna/settings.json`; see
  `docs/streaming-mode-settings.md`.
- **The wire**: deltas carry `disposition: committed|unstable` (committed adds
  `segment_index`); the server advertises `session.streaming` on the greeting.
  Contract: `specs/007-streaming-mode/contracts/streaming-wire.md`.
- **Interop fixture**: with the canonical/whisper-snap adapter + WhisperLive
  docker running on `/tmp/myna-adapter.sock`, the live protocol check is
  `cargo test -p myna-cli --test interop_canonical -- --ignored` (findings:
  `docs/interop/canonical-whisper-snap-report.md`).

Whisper streams for real (feature 008): a rolling re-decode loop decodes the
uncommitted window every `--stream-cadence-s` (default 1 s) while audio is
still arriving, and the **local-agreement** strategy commits the word prefix
two successive decodes agree on; first `~` ~1.5 s in, first `»` ~2.5 s in on
CPU (whisper-tiny watermark: +2.4 pp WER vs batch, commit stability 100%).
The 008 sweep compared three commit strategies; local-agreement was the only
one to meet the latency targets — the other two were retired (details:
`specs/008-progressive-emission/contracts/emission-semantics.md`).

Watermarks: `results/streaming-watermarks.json` (measured on 26–28 s
concatenated real-speech streams, `corpus/real/manifest-streams.json`).
Nemotron's native frame-once transducer loop remains in flight. Parakeet ships
and the sherpa-onnx snap is packaged under `specs/008-progressive-emission/`.
Details and Qwen-C deferral:
`docs/architecture/streaming.md`.

## Dictate into apps — `myna-desktop`

`myna-desktop` is the actual dictation app: activate push-to-talk and the
committed transcript is injected via **IBus** into whatever app was focused.
Because dictation targets *another* app, activation must not need terminal focus,
so the default is **toggle-to-talk via a GNOME custom keyboard shortcut**: the
app runs as a daemon and a shortcut bound to `myna-desktop --toggle` pokes it
(tap to start, tap to stop). Needs a Wayland/GNOME session and a running IBus
daemon.

```shell
(cd server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &
cd client && cargo build --release && cd ..

./client/target/release/myna-desktop --install-shortcut '<Super>t>'        # once: binds a shortcut
./client/target/release/myna-desktop --socket /tmp/myna.sock --language en   # daemon
# focus a text field, tap the shortcut, speak, tap -> transcript injected there
```

Alternatives: `--portal` (hold-to-talk via the GlobalShortcuts portal — packaged
snap/flatpak only), `--stdin` (terminal debug), `--dbus` (also serve
`org.myna.Dictation` for the GNOME Shell indicator below). See
`docs/desktop-injection.md`.

### The GNOME Shell indicator

On GNOME/Wayland a normal client can't show an always-on-top, focus-safe overlay,
so the switchable dictation indicator lives in a GJS extension inside the
compositor and reads state + audio level from `myna-desktop --dbus` over D-Bus
(`org.myna.Dictation`). Basic is the default; Wave ribbon is selectable in the
extension preferences. It never captures, transcribes, or injects. Installation
and troubleshooting: `extensions/myna-shell/README.md`; acceptance contract:
`specs/009-switchable-basic-hud/quickstart.md`.

## Benchmarking

The testbed replays a corpus through a backend and scores it offline. Accuracy is
only trustworthy on the **real** corpus (`corpus/real/`); the synthetic espeak
`server/fixtures/` exercise plumbing and latency only.

```shell
cd server
uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock &

# sweep the real corpus, tagging the run (appends to results/bench.jsonl):
uv run python ../dev/bench.py --socket /tmp/myna.sock \
    --manifest ../corpus/real/manifest.json --label whisper-base/cpu --batch

# streaming-mode runs (server needs --streaming) record extra metrics:
uv run python ../dev/bench.py --socket /tmp/myna.sock --streaming \
    --manifest ../corpus/real/manifest.json --label whisper-tiny/streaming

# collate every recorded run into a WER/CER matrix:
uv run python ../dev/aggregate.py --by-category
```

Each run also records latency from the event timeline — time-to-ready (cold model
load), time-to-first-snippet, finalize latency, and RTF — plus peak RSS/VRAM.
Streaming runs additionally record `time_to_first_committed`, `committed_segments`,
and `commit_stability` (the append-only invariant; baselines in
`results/streaming-watermarks.json`).
Pass `--cold` for the first request after a (re)start to capture the cold-load
cost distinctly.

To sweep **several backends** in one command, use the config-driven matrix runner
(provisions each target on a socket, samples a cold then a warm run, stamps
hardware provenance):

```shell
cd server
uv run python ../dev/matrix.py --config ../dev/matrix.yaml --dry-run   # show the plan
uv run python ../dev/matrix.py --config ../dev/matrix.yaml             # run it
```

WER normalisation lives in `myna.testbed.metrics` (Python-only, one source of
truth — the Rust client emits transcripts + timings that feed the same scorer).

## Tests

```shell
cd server && uv run pytest            # offline suite (skips cleanly without a model/GPU)
cd client && cargo test               # Rust workspace
```

The offline Python suite runs the fake-adapter contract tests plus each adapter's
own logic; tests that need a real model or extra skip when it's absent. Coverage:
`uv run pytest --cov=myna --cov-report=term-missing`. 100% is a non-goal — model
loaders and numpy/torch decode run only under hardware integration tests and are
deliberately left uncovered rather than mocked.

The Rust workspace has hermetic unit tests plus env-gated integration suites for
real hardware (`MYNA_PIPEWIRE_TESTS=1`, `MYNA_DBUS_TESTS=1`, IBus wire cycle) that
skip offline and run unchanged on a VM or hardware.

## Building the snaps

The `whisper` / `nemotron` extras and the snaps install the **same** third-party
libraries two independent ways: `uv sync --extra <name>` puts them in your
`.venv` for local runs; the snap build packages them into the snap so end users
need neither uv nor the extras. **You don't need to sync any extra for the snaps
to build** — snapcraft resolves them from PyPI in its own build container.

From each snap dir:

```shell
./dev/prepare.sh            # uv build --wheel -> wheels/  (no extra sync needed)
./dev/download-models.sh    # fetch model weights into components/  (large)
snapcraft pack
```

Per-snap details: [`whisper-snap/README.md`](whisper-snap/README.md),
[`nemotron-snap/README.md`](nemotron-snap/README.md),
[`qwen-snap/README.md`](qwen-snap/README.md),
[`parakeet-snap/README.md`](parakeet-snap/README.md), and
[`sherpa-snap/README.md`](sherpa-snap/README.md).

## Contributing

Development is spec-driven (spec-kit, adopted mid-2026): new features are
specified, planned, and broken into tasks under `specs/NNN-*/` before
implementation, governed by the project constitution
([`.specify/memory/constitution.md`](.specify/memory/constitution.md)) — TDD for
shipped Rust components, integration tests that run on a VM and on hardware
unchanged, performance watermarks, the Workshop dev env, and privacy/offline
invariants. If you're extending the project, start with the `speckit-specify`
workflow rather than editing code directly, and read `docs/project-plan.md` for
where things stand.
