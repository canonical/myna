# myna
Speech to text on Ubuntu

Myna is a lightweight speech-to-text application for Ubuntu Desktop.

The project draws its name from the [myna](https://en.wikipedia.org/wiki/Myna), a bird renowned for its ability to listen to, mimic, and reproduce human speech with astonishing clarity. Just like its avian counterpart, this application is designed to master voice audio-listening intently to your spoken words and instantly translating them into accurate, clean text. Whether you are looking to dictate text hands-free, improve accessibility, or streamline your workflow, myna brings seamless voice recognition directly to your Linux ecosystem.

## Repository layout

- `src/myna/core` — shared vocabulary: audio types, transcript events, session config, transports (loopback + WebSocket/UDS). Includes `wire_ie115.py`, the OpenAI-Realtime **IE115** wire dialect codec.
- `src/myna/testbed` — candidate-adapter evaluation testbed (fake + model adapters, harness, fixture corpus, metrics)
- `src/myna/server` — standalone UbuSTT server (`myna-server`), the process the snaps ship
- `src/myna/desktop` — interface stubs for the Ubuntu Desktop dictation client (UD129)
- `rust/` — the dictation **client** and orchestrator: a wire-agnostic session/residency FSM (`myna-orchestrator`) + the `myna-dictate` push-to-talk binary (`myna-cli`), speaking both the internal wire and IE115
- `whisper-snap/`, `nemotron-snap/`, `qwen-snap/` — one inference snap per model family (engines/runtimes/models + modelctl), strict-confinement
- `docs/architecture` — architecture decision records; read before structural changes
- `docs/project-plan.md` — workstreams, tasks, and milestones (the living tracker)
- `reference/` — local checkouts of related projects (inference snaps, CLI); not committed

## Architecture: three roles, one contract

Everything speaks one **session contract** (audio-push in, transcript events out;
`docs/architecture/transport-and-events.md`) over WebSocket-on-a-Unix-socket.
Three processes play three roles against it:

- **Inference backend** — the Python `myna-server` (and the snaps that wrap it).
  Hosts a model, listens on a UDS, has *no* microphone: the client pushes PCM,
  the server rejects off-format audio and never resamples. Swap models with
  `--adapter whisper|nemotron|qwen-c|fake`.
- **Dictation client** — the Rust `myna-dictate` (`rust/`). Owns capture and the
  hotkey, runs the wire-agnostic session/residency FSM, injects transcribed text.
  The production hot path.
- **Benchmark client** — the Python `dev/bench.py`. Replays a corpus through a
  backend and scores WER/CER offline (no latency constraint), so accuracy work
  and the dictation hot path don't entangle.

Two peers (Rust client, Python bench) drive the *same* backend; a backend serves
either. The wire is a **selectable dialect** — the internal flat
`transcription.*` vocab, or the OpenAI-Realtime-shaped **IE115** names — and both
ends translate at the edge, so neither the models nor the FSM change when the
dialect does (`docs/architecture/ie115-wire.md`).

## Development

Tooling is [uv](https://docs.astral.sh/uv/). `uv` owns a project virtualenv
(`.venv/`) and keeps it matching `pyproject.toml` + `uv.lock`: `uv sync`
installs that environment, `uv run <cmd>` runs a command inside it.

### Dependencies and extras

The base install is tiny (`websockets`) plus the `dev` group (pytest,
pytest-cov, Hypothesis). The two real ASR backends are **optional extras** — opt
in only when you need them, because they are large:

| Extra | Pulls in | For |
|---|---|---|
| `whisper`  | faster-whisper (CTranslate2) | the Whisper adapter / `whisper-snap` |
| `nemotron` | `nemo_toolkit[asr]` + torch + CUDA (multi-GB) | the Nemotron adapter / `nemotron-snap` |

`uv sync` is declarative: it makes `.venv` match *exactly* what you request, so
it installs no extras by default, and syncing with fewer extras prunes ones
installed earlier. Name every extra you want in the same command:

```shell
uv sync                                     # base + dev only (fake adapter, contract tests)
uv sync --extra whisper                      # + Whisper
uv sync --extra whisper --extra nemotron     # + both real adapters
uv sync --all-extras                         # + everything
```

A `ModuleNotFoundError` for `faster_whisper` / `nemo` just means that extra
isn't synced into the current `.venv`. Model **weights** are separate: they
download to `HF_HOME` on first use of an adapter (verify offline with
`HF_HUB_OFFLINE=1`).

The `qwen-c` adapter needs **no** pip extra — it's a pure-C engine reached via
ctypes. Point it at the shared library and a local model dir:
`QWEN_ASR_LIB=/snap/qwen/current/lib/libqwen_asr.so uv run myna-server
--adapter qwen-c --model qwen-snap/components/Qwen3-ASR-0.6B --socket /tmp/ubustt.sock`.

### Common commands

```shell
uv run pytest                            # offline suite: contract + adapter logic
uv run python -m myna.testbed            # demo: fake adapter (--transport ws for UDS)
uv run python dev/generate_fixtures.py   # synthetic corpus -> fixtures/  (needs libespeak-ng1 + espeak-ng-data)
uv run python dev/fetch_real_corpus.py   # real LibriSpeech corpus -> corpus/real/  (needs ffmpeg; ~337 MB download)

# Serve a real adapter on a Unix socket, then talk to it:
uv run myna-server --adapter nemotron --socket /tmp/ubustt.sock   # or --adapter whisper | qwen-c
uv run python dev/capabilities.py --socket /tmp/ubustt.sock       # what can this server do?
uv run python dev/transcribe.py --socket /tmp/ubustt.sock quiet-weather   # transcribe a fixture clip
```

Both corpora are generated, not committed (gitignored) — run the builders above
once before corpus-backed runs.

### Dictate (the Rust client)

`myna-dictate` is the push-to-talk client: it owns capture and drives the
session FSM against a running backend. Build and run it against any
`myna-server`:

```shell
cd rust && cargo build --release && cd ..
uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock &

# real-time push-to-talk from a WAV clip (Enter to start, Enter/clip-end to stop):
./rust/target/release/myna-dictate --socket /tmp/ubustt.sock --language en \
    --clip corpus/real/audio/<id>.wav

# same run over the OpenAI-Realtime IE115 wire — same FSM, different dialect:
./rust/target/release/myna-dictate --socket /tmp/ubustt.sock --dialect ie115 \
    --language en --clip corpus/real/audio/<id>.wav
```

For a live-mic demo without Rust, `uv run python dev/dictate.py --socket
/tmp/ubustt.sock` drives the same backend from Python (PipeWire capture).

### Evaluate (benchmarking — the north star)

The testbed replays a corpus through a backend and scores it offline. Accuracy
is only trustworthy on the **real** corpus (`corpus/real/`, recorded speech);
the synthetic espeak `fixtures/` exercise plumbing and latency only.

```shell
uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock &

# sweep the real corpus, tagging the run; appends to results/bench.jsonl
uv run python dev/bench.py --socket /tmp/ubustt.sock \
    --manifest corpus/real/manifest.json --label whisper-base/cpu --batch

# aggregate every recorded run into a WER/CER matrix (optionally per UD129 category)
uv run python dev/aggregate.py --by-category
```

Repeat the `bench.py` run per adapter/model/machine (each `--label` is a row);
`aggregate.py` collates them across the lab. WER normalisation lives in
`myna.testbed.metrics` and is deliberately Python-only — the Rust client emits
transcripts + timings that feed the *same* scorer, keeping one source of truth.

Beyond WER/CER, each run records latency drawn from the event timeline:
**time-to-ready** (the cold model-load wait, `preparing`→`ready`), time-to-first
snippet, finalize latency (key-release→committed text), and **RTF** (decode ÷
audio, in `--batch`). Pass `--cold` for the first request after a (re)start so
the snap's model-load-from-cold cost is captured distinctly; `aggregate.py`
splits cold from warm and reports p50/p95.

To sweep **several backends** in one command, use the config-driven matrix
runner. It provisions each target on a socket, takes a cold then a warm sample,
stamps hardware provenance, and prints the matrix:

```shell
uv run python dev/matrix.py --config dev/matrix.yaml --dry-run   # show the plan
uv run python dev/matrix.py --config dev/matrix.yaml             # run it
```

Edit `dev/matrix.yaml`: the `hardware:` block (machine/cpu/gpu/tier — what makes
cross-lab aggregation meaningful) and the `targets:`. A target is provisioned
either by **`server`** (the runner spawns `myna-server` itself — no snap, no
sudo; the local-first path) or **`snap`** (drives an installed snap: switches its
engine/model and restarts it to force a cold load). The example config runs
whisper/qwen-c/nemotron locally with the `server` provisioner.

### Tests and coverage

`uv run pytest` runs the **offline** suite: the fake-adapter contract tests plus
each adapter's own logic (audio-format rejection, event finalisation, NeMo
result-shape handling). Tests that need a real model or extra skip cleanly when
it's absent, so the offline run stays green on any machine — including in CI
without a GPU.

A few invariants are checked with [Hypothesis](https://hypothesis.readthedocs.io/)
(property-based testing) rather than hand-picked examples — e.g. that *any*
non-canonical audio format is rejected, and that the transcript is recovered
from every NeMo return shape. Hypothesis caches counterexamples under
`.hypothesis/` (gitignored).

Coverage uses [pytest-cov](https://pytest-cov.readthedocs.io/):

```shell
uv run pytest --cov=myna                                  # coverage summary for the package
uv run pytest --cov=myna --cov-report=term-missing        # + the exact uncovered line numbers
uv run pytest --cov=myna --cov-report=html                # browsable report -> htmlcov/index.html
uv run pytest --cov=myna.testbed.nemotron tests/test_nemotron_unit.py   # scope to one module
```

100% is a non-goal. The model loaders (the real NeMo/CTranslate2 import and
checkpoint restore) and the numpy/torch decode run only under the hardware
integration tests, which skip offline; they are marked `# pragma: no cover` or
left uncovered **deliberately** rather than mocked — the reported number then
reflects logic that is genuinely exercised, not assertions against a mock.

### Extras vs. the snaps

The `whisper` / `nemotron` extras and the snap packages install the **same
third-party libraries** (faster-whisper, NeMo) — they're declared once in
`pyproject.toml` and consumed two independent ways:

- `uv sync --extra <name>` puts them in your `.venv` so you can run an adapter
  locally (testbed / `myna-server`).
- the snap build packages them *into the snap*, so end users need neither uv
  nor the extras.

**You do not need to sync (or `uv add`) any extra for the snaps to build.** The
build is independent of your `.venv`: `dev/prepare.sh` runs `uv build --wheel`
(which only *packages* the project and its extra definitions), then `snapcraft`
runs `pip install ./myna-…whl[whisper]` inside its own isolated build container,
resolving the extra from PyPI itself. A fresh checkout builds cleanly — expect a
long wait while snapcraft pulls torch/CUDA/NeMo and `download-models.sh` fetches
weights.

Per-snap build/install steps: [`whisper-snap/README.md`](whisper-snap/README.md),
[`nemotron-snap/README.md`](nemotron-snap/README.md),
[`qwen-snap/README.md`](qwen-snap/README.md). In short, from each snap dir:

```shell
./dev/prepare.sh            # uv build --wheel -> wheels/  (no extra sync needed)
./dev/download-models.sh    # fetch model weights into components/  (large)
snapcraft pack
```
