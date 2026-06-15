# myna
Speech to text on Ubuntu

Myna is a lightweight speech-to-text application for Ubuntu Desktop.

The project draws its name from the [myna](https://en.wikipedia.org/wiki/Myna), a bird renowned for its ability to listen to, mimic, and reproduce human speech with astonishing clarity. Just like its avian counterpart, this application is designed to master voice audio-listening intently to your spoken words and instantly translating them into accurate, clean text. Whether you are looking to dictate text hands-free, improve accessibility, or streamline your workflow, myna brings seamless voice recognition directly to your Linux ecosystem.

## Repository layout

- `src/myna/core` — shared vocabulary: audio types, transcript events, session config, transports (loopback + WebSocket/UDS)
- `src/myna/testbed` — candidate-adapter evaluation testbed (fake + model adapters, harness, fixture corpus, metrics)
- `src/myna/server` — standalone UbuSTT server (`myna-server`), the process the snap's ship
- `src/myna/desktop` — interface stubs for the Ubuntu Desktop dictation client (UD129)
- `whisper-snap/` — Whisper inference snap packaging (engines/runtimes/models + modelctl)
- `nemotron-snap/` — Nemotron / FastConformer inference snap packaging (engines/runtimes/models + modelctl)
- `docs/architecture` — architecture decision records; read before structural changes
- `docs/project-plan.md` — workstreams, tasks, and milestones
- `reference/` — local checkouts of related projects (inference snaps, CLI); not committed

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

### Common commands

```shell
uv run pytest                            # offline suite: contract + adapter logic
uv run python -m myna.testbed            # demo: fake adapter (--transport ws for UDS)
uv run python dev/generate_fixtures.py   # synthetic corpus -> fixtures/  (needs libespeak-ng1 + espeak-ng-data)
uv run python dev/fetch_real_corpus.py   # real LibriSpeech corpus -> corpus/real/  (needs ffmpeg; ~337 MB download)

# Serve a real adapter on a Unix socket, then talk to it:
uv run myna-server --adapter nemotron --socket /tmp/ubustt.sock   # or --adapter whisper
uv run python dev/capabilities.py --socket /tmp/ubustt.sock       # what can this server do?
uv run python dev/transcribe.py --socket /tmp/ubustt.sock quiet-weather   # transcribe a fixture clip
```

Both corpora are generated, not committed (gitignored) — run the builders above
once before corpus-backed runs.

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
[`nemotron-snap/README.md`](nemotron-snap/README.md). In short, from each snap dir:

```shell
./dev/prepare.sh            # uv build --wheel -> wheels/  (no extra sync needed)
./dev/download-models.sh    # fetch model weights into components/  (large)
snapcraft pack
```
