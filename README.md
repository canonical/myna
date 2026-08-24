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
sudo apt install build-essential ffmpeg libpipewire-0.3-dev libclang-dev libgio-2.0-dev libgdk-pixbuf-2.0-dev libgtk-4-dev
```

The fastest smoke test needs no model weights and no audio — the `fake` adapter
serves a scripted transcript over the in-process loopback transport:

```shell
cd server && uv sync && uv run python -m myna.testbed   # fake adapter -> a ResultRecord
```

To dictate for real, a Myna STT inference snap needs to be installed (or run one from this repo directly). You can then use the reference client to push audio at it and see transcription results. The LibriSpeech corpus is a good example (`uv run python ../dev/fetch_real_corpus.py`).

```shell
# 1. serve Whisper on a Unix socket (downloads the `base` weights on first session)
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
  testbed/demo client; `myna-desktop` is the shipped dictation app
  (hotkey → capture → IBus injection into the focused app). `myna-audio` is a
  PipeWire capture adapter, `myna-orchestrator` the session FSM, `myna-core` the
  wire contract.
- `extensions/myna-shell/` — a **GJS** GNOME Shell extension: a focus-safe,
  animated dictation indicator that runs inside the compositor and consumes state
  from `myna-desktop` over D-Bus. Easy to make your own.
- `*-snap/` — one inference snap per model family (strict-confinement packages
  of `myna-server` + a model + engines).
- `docs/` — architecture and design notes; `docs/project-plan.md` is the living
  task tracker.

## How it fits together

Everything speaks one **session contract** — audio-push in, transcript events
out — over WebSocket-on-a-Unix-socket. Three roles play against it:

- **Inference backend** (`myna-server` / a snap): hosts a model, listens on a
  socket, has *no* microphone — the client pushes PCM and the server rejects
  off-format audio rather than resampling. Swap models with
  `--adapter whisper|nemotron|qwen-c|...|fake` (adapter comparison:
  [docs/asr-inference-snap-design.md](docs/asr-inference-snap-design.md) §7).
- **Dictation client** (`client/`): owns capture and the hotkey and runs
  the session FSM. `myna-dictate` is the demo (WAV/corpus/live-mic);
  `myna-desktop` is the shipped app that injects into the focused app via IBus.
- **Benchmark client** (`dev/bench.py`): replays a corpus through a backend and
  scores WER/CER offline, so accuracy work stays out of the dictation hot path.

The wire is a **selectable dialect** — the internal `transcription.*`
vocabulary or the **Myna STT API** (a compatibile subset of the OpenAI Realtime
Transcription API, with additions). Referred to as IE115 internally — and both
ends translate at the edge, so neither the models nor the FSM change when the
dialect does (`docs/architecture/ie115-wire.md`).

## Development environment

### Workshop (recommended)

A [Canonical Workshop](https://ubuntu.com/workshop/docs) definition in
[`.workshop/`](.workshop/) gives you the whole toolchain (Rust, PipeWire deps,
`uv`) in one reproducible environment — the same one CI uses.

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
uv sync --extra whisper                     # packages just of the whisper server
uv sync --all-extras                        # packages for every packaged model server (BIG)
```

`uv sync` is declarative — it makes `.venv` match *exactly* what you request, and
syncing with fewer extras prunes ones installed earlier.

Model **weights** are separate from the code: they download to `HF_HOME` on first
use of an adapter (verify offline with `HF_HUB_OFFLINE=1`).

## Common commands

```shell
cd server
uv run pytest                                   # offline suite: contract + adapter logic
uv run python -m myna.testbed                   # demo: fake adapter over loopback
uv run python ../dev/generate_fixtures.py       # synthetic corpus -> server/fixtures/
uv run python ../dev/fetch_real_corpus.py       # real LibriSpeech corpus -> corpus/real/
uv run python ../dev/fetch_chinese_corpus.py    # FLEURS Mandarin corpus  -> corpus/chinese/

# serve a real adapter, then talk to it:
uv run myna-server --adapter nemotron --socket /tmp/myna.sock
uv run python ../dev/capabilities.py --socket /tmp/myna.sock    # what can this server do?
uv run python ../dev/transcribe.py --socket /tmp/myna.sock quiet-weather   # a fixture clip
```

All corpora are generated, not committed - run the builders once before
corpus-backed runs. See [Benchmarking](#benchmarking) for which real tier to
sweep for which question.

## The dictation client

`myna-dictate` owns capture and drives the session FSM against any running
backend:

```shell
cd client && cargo build --release && cd ..
(cd server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &

# real-time from a WAV clip:
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en \
    --clip corpus/real/audio/<id>.wav

# over the Myna STT wire — same FSM, different dialect:
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --language en --clip corpus/real/audio/<id>.wav

# live microphone:
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en --mic
./client/target/release/myna-dictate --socket /tmp/myna.sock --language en --mic \
    --target alsa_input.pci-0000_c1_00.6.HiFi__Mic2__source   # a specific node.name

# list input devices live (stable node.name + label):
./client/target/release/myna-dictate --list-devices
```

## Streaming mode

By default dictation is **batch**: text appears when the utterance ends. In a streaming
mode, dictation results come back as you speak.

Not all models are designed for streaming, and the feature may require more powerful
hardware than batch mode. The backends that do support it can be switched by providing
a `--streaming` parameter. The streaming chunks may always be committed (injected). Provisional
hypothesis text is marked as `unstable` and should never be injected; it only displays if you opt in.

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

## Dictate into apps — `myna-desktop`

`myna-desktop` is the actual dictation app: activate it and the
committed transcript is injected via **IBus** into whatever app was focused.
Because dictation targets *another* app, activation must not need terminal focus,
so the default is **toggle-to-talk via a GNOME custom keyboard shortcut**: the
app runs as a daemon and a shortcut bound to `myna-desktop --toggle` pokes it
(tap to start, tap to stop). Needs a Wayland/GNOME session and a running IBus
daemon.

```shell
(cd server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &
cd client && cargo build --release && cd ..

./client/target/release/myna-desktop --install-shortcut '<Super>t'           # once: binds a shortcut
./client/target/release/myna-desktop --socket /tmp/myna.sock --language en   # daemon
# focus a text field, tap the shortcut, speak, tap -> transcript injected there
```

The daemon resolves its own wiring: activation follows packaging (control
socket here, the GlobalShortcuts portal when `$SNAP` is set, since only a
packaged app gets a portal app identity), `org.myna.Dictation` is always
served for the indicator below with a notification fallback, and in-field
preedit follows the streaming tier gate. Force any of them with `--portal` /
`--control` / `--stdin`, `--no-dbus`, `--preedit` / `--no-preedit`;
`--hold` makes portal activation hold-to-talk. See
`docs/desktop-injection.md`.

### The GNOME Shell indicator

On GNOME/Wayland a normal client can't show an always-on-top, focus-safe overlay,
so the animated dictation indicator lives in a GJS extension that runs inside the
compositor and reads state + audio level from `myna-desktop` over D-Bus
(`org.myna.Dictation`). It never captures, transcribes, or injects.

To iterate on the extension without rebuilding the package, put the symlink
where the package would land - and remove it before installing the real deb,
which claims the same path:

```shell
sudo ln -sfn "$PWD/extensions/myna-shell" \
    /usr/share/gnome-shell/extensions/myna-shell@canonical.com
# restart the shell: log out/in (Wayland)
```

See also `specs/004-gnome-shell-indicator/quickstart.md`.

## Benchmarking

The testbed replays a corpus through a backend and scores it offline. Accuracy is
only trustworthy on the **real** corpora (`corpus/real/`, `corpus/chinese/`); the
synthetic espeak `server/fixtures/` exercise plumbing and latency only.

The real tiers, and what each is for:

| Manifest | Clips | Audio | Use it for |
| --- | --- | --- | --- |
| `corpus/real/manifest-balanced.json` | 82 | ~12 min | **English accuracy, clean.** Round-robin over all 40 LibriSpeech dev-clean speakers - the only English tier whose WER is a property of the language rather than of one voice. |
| `corpus/librispeech-other/manifest-balanced.json` | 82 | ~13 min | **English accuracy, hard.** Same construction over LibriSpeech test-other: accented, noisier, lower-fidelity recordings. Report the clean/other pair together - the gap between them is what separates backends. |
| `corpus/real/manifest.json` | 14 | ~1 min | Quick smoke runs, and continuity with older `results/*.jsonl` (single speaker, 2277). |
| `corpus/real/manifest-streams.json` | 2 | ~55 s | Long-form streaming watermarks. **Frozen** - `results/streaming-watermarks.json` and the emission-invariant tests are baselined against these exact clips. |
| `corpus/chinese/manifest.json` | 50 | ~9 min | Mandarin CER (FLEURS `cmn_hans_cn` test), for SenseVoice/FunASR against published figures. |

Rebuild any of them from the cached downloads (no network needed once `.cache/`
is populated):

```shell
uv run python dev/fetch_real_corpus.py --select balanced -n 80 \
    --manifest-name manifest-balanced.json     # speaker-balanced English (clean)
uv run python dev/fetch_real_corpus.py --subset test-other --select balanced -n 80 \
    --out corpus/librispeech-other --manifest-name manifest-balanced.json  # (hard)
uv run python dev/fetch_real_corpus.py                 # the original 14-clip tier
uv run python dev/fetch_chinese_corpus.py -n 50        # Mandarin
```

One LibriSpeech split per corpus dir - the `NOTICE` carries that split's
attribution, so the builder refuses to mix them.

```shell
cd server
uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock &

# sweep the real corpus, tagging the run (appends to results/bench.jsonl):
uv run python ../dev/bench.py --socket /tmp/myna.sock \
    --manifest ../corpus/real/manifest-balanced.json --label whisper-base/cpu --batch

# streaming-mode runs (server needs --streaming) record extra metrics:
uv run python ../dev/bench.py --socket /tmp/myna.sock --streaming \
    --manifest ../corpus/real/manifest-balanced.json --label whisper-tiny/streaming

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
cd client && cargo test               # desktop client suite
```

The offline Python suite runs the fake-adapter contract tests plus each adapter's
own logic; tests that need a real model or extra skip when it's absent. Coverage:
`uv run pytest --cov=myna --cov-report=term-missing`. 100% is a non-goal — model
loaders and numpy/torch decode run only under hardware integration tests and are
deliberately left uncovered rather than mocked.

The Rust workspace has hermetic unit tests plus env-gated integration suites for
real hardware (`MYNA_PIPEWIRE_TESTS=1`, `MYNA_DBUS_TESTS=1`, IBus wire cycle) that
skip offline and run unchanged on a VM or hardware.

### Coverage and dead code

One command per language in the Workshop environment (same commands in CI):

```shell
workshop run myna cov        # Rust: HTML + lcov + Cobertura under client/target/coverage/
workshop run myna py-cov     # Python: htmlcov/ + term-missing + Cobertura
workshop run myna exercise   # real use-cases (both wire dialects) instrumented, merged exports
workshop run myna deadcode   # populations + dead-code report: client/target/coverage/populations.md
```

`deadcode`'s report classifies every line as **test-covered**,
**use-case-only** (an integration-test gap, not dead code), or
**never-executed**, and appends static findings (`cargo machete`, vulture,
ruff F401/F841). CI additionally enforces a **patch-coverage gate** on every
PR: 80% of changed coverable lines (5-line floor; deletion-only PRs can't
false-fail), self-hosted - no external service (`workshop run myna patch-cov`
locally). Whole-project coverage is reported informationally only.

### Instrumented builds & manual coverage

To answer "did my manual testing exercise this code?", run any component
instrumented, poke at it by hand, then generate the report. The scripted
actions and manual runs write to the **same** data locations, so any mix
merges into one report.

Rust (per run, from `client/`):

```shell
cargo llvm-cov run --no-report --bin myna-dictate -- --socket /tmp/myna.sock --mic
# ... poke at it; repeat with --bin myna-desktop etc. Each run appends data.
cargo llvm-cov report --html --output-dir target/coverage/html   # report on demand
cargo llvm-cov clean --workspace                                 # reset
```

Python (server/):

```shell
uv run coverage run --parallel-mode --context=usecase:manual -m myna.server --adapter fake --socket /tmp/myna.sock
# ... drive sessions against it; stop with Ctrl-C when done
uv run coverage combine          # merge all runs (tests + every manual session)
uv run coverage html --show-contexts   # per-line "covered by which run" detail
rm -f .coverage*                 # reset
```

To fold your manual runs into the populations + dead-code report, refresh
the merged exports and run `deadcode`:

```shell
cd client && cargo llvm-cov report --cobertura --output-path target/coverage/rust-merged.cobertura.xml
cd ../server && uv run coverage xml -o coverage-merged.cobertura.xml
cd .. && workshop run myna deadcode   # or: python dev/coverage_populations.py
```

### Quality gates

CI runs the full static battery per PR, each also runnable locally as a
Workshop action: `fmt`, `machete` (unused Rust deps), `deny` (dependency
policy - mechanically bans HTTP/TLS/cloud client crates in the client,
codifying the offline invariant), `py-lint` (ruff), `py-types` (mypy strict
on `myna/core`), `shell-lint`, `workflow-lint`. Dependency-advisory audits
(`workshop run myna audit`) run weekly, not per PR.

## Building the snaps

The server extras and the inference snaps pacakged here install the **same** third-party
libraries two independent ways: `uv sync --extra <name>` puts them in your
`.venv` for local runs; the snap build packages them into the snap. **You don't need to sync any extra for the snaps to build** — snapcraft resolves them from PyPI in its own build container.

One target per snap, named for the model - it fetches the model weights into
`components/` (large, but skipped if already there), stages the wheels and
packs:

```shell
make snap-whisper           # or snap-parakeet, snap-qwen, snap-myna, snap-fake, ...
```

That runs the snap's own `dev/prepare.sh` (`uv build --wheel` -> `wheels/`) and
its model fetch; `make help` lists every snap target.

The snaps have their own README's with further details.

## Configuration UI prototype

`dev/config-ui.py` is a **throwaway Tkinter prototype**, not a shipped surface
and not a design commitment. It exists to explore the questions in
[docs/configuration-api.md](docs/configuration-api.md) against real installed
snaps: what is actually configurable today, what a Settings panel has to guess
in the absence of a config schema, and what the running system costs.

```shell
./dev/config-ui.py          # stdlib only, no deps beyond python3-tk
```

It discovers backends by their `ubustt-socket` content slot, reads them
unprivileged (`<backend> get|status|list-models|list-engines`), shows live
service state, resident/peak memory, CPU time and on-disk size per backend
alongside the client's dictation state from `org.myna.Dictation`, and writes
through `pkexec`, always displaying the literal command first.

What it surfaces, and what a real config API would have to answer:

- No machine-readable schema exists, so every control is rendered from the
  *current value*: types are inferred, and ranges, defaults, titles and
  restart-required flags are guessed. This is the case for `describe-config`
  (config-api §3.4, Appendix A) in one screen.
- The uniform-vocabulary split is already visible: parakeet exposes four
  `stream-*` keys that funasr does not, and nothing distinguishes "not
  applicable here" from "not implemented yet".
- There is no *active backend* concept. Several backends can be connected to
  the client's single `backend` plug, and an unconnected backend still runs and
  holds its model resident. Connection state is configuration, and it lives on
  the client snap rather than on the inference snaps.
- Residency reads better as intent next to a live number ("2.6G resident, idle
  since 13:27") than as `sleep-idle-seconds`, which supports the `intent`
  presets sketched in Appendix A.

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

See: https://github.com/github/spec-kit
