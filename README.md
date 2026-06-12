# myna
Speech to text on Ubuntu

Myna is a lightweight speech-to-text application for Ubuntu Desktop.

The project draws its name from the [myna](https://en.wikipedia.org/wiki/Myna), a bird renowned for its ability to listen to, mimic, and reproduce human speech with astonishing clarity. Just like its avian counterpart, this application is designed to master voice audio-listening intently to your spoken words and instantly translating them into accurate, clean text. Whether you are looking to dictate text hands-free, improve accessibility, or streamline your workflow, myna brings seamless voice recognition directly to your Linux ecosystem.

## Repository layout

- `src/myna/core` — shared vocabulary: audio types, transcript events, session config, transports (loopback + WebSocket/UDS)
- `src/myna/testbed` — candidate-adapter evaluation testbed (fake + faster-whisper adapters, harness, fixture corpus, metrics)
- `src/myna/server` — standalone UbuSTT server (`myna-server`), the process the whisper snap ships
- `src/myna/desktop` — interface stubs for the Ubuntu Desktop dictation client (UD129)
- `whisper-snap/` — Whisper inference snap packaging (engines/runtimes/models + modelctl)
- `docs/architecture` — architecture decision records; read before structural changes
- `docs/project-plan.md` — workstreams, tasks, and milestones
- `reference/` — local checkouts of related projects (inference snaps, CLI); not committed

## Development

Tooling is [uv](https://docs.astral.sh/uv/):

```shell
uv sync                                  # install
uv run pytest                            # contract tests (loopback + WebSocket/UDS)
uv run python -m myna.testbed            # demo: fake adapter (--transport ws for UDS)
uv run python dev/generate_fixtures.py   # synthesize the fixture corpus into fixtures/
                                         # (needs libespeak-ng1 + espeak-ng-data)
uv sync --extra whisper                  # adds faster-whisper for the real adapter

```
